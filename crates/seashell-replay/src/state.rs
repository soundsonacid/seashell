//! The accounts taken out of a snapshot: the file they are kept in, and loading them into a
//! [`Seashell`] as accounts, sysvars, the feature set and programs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use agave_feature_set::{FeatureSet, FEATURE_NAMES};
use seashell::Seashell;
use seashell_snapshot::SnapshotAccount;
use serde::{Deserialize, Serialize};
use solana_account::{AccountSharedData, WritableAccount};
use solana_compute_budget::compute_budget::ComputeBudget;
use solana_program_runtime::program_cache::metrics::LoadProgramMetrics;
use solana_program_runtime::program_cache::entry::{ProgramCacheEntry, ProgramCacheEntryType};
use solana_pubkey::Pubkey;
use solana_sdk_ids::{
    bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, loader_v4, sysvar,
};
use solana_syscalls::create_program_runtime_environment;

/// An account as kept in the accounts file.
#[derive(Serialize, Deserialize)]
pub struct StoredAccount {
    pub address: [u8; 32],
    pub slot: u64,
    pub lamports: u64,
    pub owner: [u8; 32],
    pub executable: bool,
    pub rent_epoch: u64,
    pub data: Vec<u8>,
}

/// Everything the replay needs from the snapshot for transactions that name `addresses`: those
/// accounts, the program data account each one would have if it were an upgradeable program,
/// every feature account and the sysvars.
pub fn wanted_addresses(addresses: &HashSet<Pubkey>) -> HashSet<[u8; 32]> {
    let program_data = addresses.iter().map(|address| {
        Pubkey::find_program_address(&[address.as_ref()], &bpf_loader_upgradeable::id()).0
    });
    let sysvars = [
        sysvar::clock::id(),
        sysvar::epoch_schedule::id(),
        sysvar::epoch_rewards::id(),
        sysvar::rent::id(),
        sysvar::slot_hashes::id(),
        sysvar::stake_history::id(),
        sysvar::last_restart_slot::id(),
    ];
    addresses
        .iter()
        .copied()
        .chain(program_data)
        .chain(FEATURE_NAMES.keys().copied())
        .chain(sysvars)
        .map(|address| address.to_bytes())
        .collect()
}

pub fn write_accounts(path: &Path, found: HashMap<[u8; 32], SnapshotAccount>) {
    let accounts: Vec<StoredAccount> = found
        .into_iter()
        .map(|(address, account)| StoredAccount {
            address,
            slot: account.slot,
            lamports: account.lamports,
            owner: account.owner,
            executable: account.executable,
            rent_epoch: account.rent_epoch,
            data: account.data,
        })
        .collect();
    let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    bincode::serialize_into(file, &accounts).expect("failed to write the accounts file");
}

/// The live accounts of the accounts file. A zero-lamport entry is a deleted account.
pub fn read_accounts(path: &Path) -> HashMap<Pubkey, AccountSharedData> {
    let file = std::io::BufReader::new(
        std::fs::File::open(path).unwrap_or_else(|error| panic!("{}: {error}", path.display())),
    );
    let accounts: Vec<StoredAccount> =
        bincode::deserialize_from(file).expect("failed to read the accounts file");
    accounts
        .into_iter()
        .filter(|account| account.lamports > 0)
        .map(|stored| {
            let mut account = AccountSharedData::new_with_data(
                stored.lamports,
                stored.data,
                &Pubkey::from(stored.owner),
            );
            account.set_executable(stored.executable);
            account.set_rent_epoch(stored.rent_epoch);
            (Pubkey::from(stored.address), account)
        })
        .collect()
}

/// The features whose accounts say they are activated.
///
/// A feature account's data is a `Feature`: an optional activation slot, as one byte (1 when
/// present) followed by the slot.
fn feature_set(accounts: &HashMap<Pubkey, AccountSharedData>) -> FeatureSet {
    let mut feature_set = FeatureSet::default();
    for feature_id in FEATURE_NAMES.keys() {
        let Some(account) = accounts.get(feature_id) else {
            continue;
        };
        if let [1, slot @ ..] = solana_account::ReadableAccount::data(account) {
            if let Some(slot) = slot.first_chunk::<8>() {
                feature_set.activate(feature_id, u64::from_le_bytes(*slot));
            }
        }
    }
    feature_set
}

/// What a replay starts from, taken from a seashell right after the snapshot was loaded.
pub struct Start {
    pub accounts: HashMap<Pubkey, AccountSharedData>,
    /// The clock's timestamp at the snapshot slot.
    pub timestamp: i64,
}

impl Start {
    pub fn of(seashell: &Seashell) -> Self {
        Start {
            accounts: seashell.accounts_db.accounts.read().clone(),
            timestamp: seashell.accounts_db.sysvars.clock().unix_timestamp,
        }
    }

    /// Puts the accounts back as they were at the snapshot.
    pub fn restore_accounts(&self, seashell: &Seashell) {
        *seashell.accounts_db.accounts.write() = self.accounts.clone();
    }
}

pub struct LoadReport {
    pub accounts: usize,
    pub active_features: usize,
    pub known_features: usize,
    pub programs_loaded: usize,
    /// How many of the loaded programs are of each SBPF version.
    pub programs_by_version: BTreeMap<String, usize>,
    /// Programs that could not be loaded, and why.
    pub programs_failed: Vec<(Pubkey, String)>,
}

/// A seashell holding `accounts`: its feature set and sysvars are the snapshot's, and every
/// executable account among them is a loaded program.
pub fn seashell_from_accounts(
    accounts: &HashMap<Pubkey, AccountSharedData>,
) -> (Seashell, LoadReport) {
    let mut seashell = Seashell::default();
    seashell.config.memoize = true;
    seashell.feature_set = feature_set(accounts);
    let active_features = FEATURE_NAMES
        .keys()
        .filter(|feature_id| seashell.feature_set.is_active(feature_id))
        .count();
    seashell.compute_budget = ComputeBudget::new_with_defaults(
        seashell
            .feature_set
            .is_active(&agave_feature_set::raise_cpi_nesting_limit_to_8::id()),
    );

    // Builtins and precompiles first, so that the snapshot's accounts and programs replace the
    // placeholders where a builtin has since been migrated to an SBF program.
    seashell.accounts_db.load_builtins(&seashell.feature_set);
    seashell.load_precompiles();
    for (address, account) in accounts {
        seashell.accounts_db.set_account(*address, account.clone());
    }

    let program_runtime_environment = create_program_runtime_environment(
        &seashell.feature_set.runtime_features(),
        &seashell.compute_budget.to_budget(),
        false,
        false,
    )
    .expect("failed to create the program runtime environment");
    let mut programs_loaded = 0;
    let mut programs_by_version: BTreeMap<String, usize> = BTreeMap::new();
    let mut programs_failed = Vec::new();
    for (address, account) in accounts {
        use solana_account::ReadableAccount;
        if !account.executable() {
            continue;
        }
        let elf = match program_elf(account, accounts) {
            Ok(Some(elf)) => elf,
            Ok(None) => continue,
            Err(why) => {
                programs_failed.push((*address, why.to_string()));
                continue;
            }
        };
        match ProgramCacheEntry::load(
            account.owner(),
            program_runtime_environment.clone(),
            0,
            elf,
            &mut LoadProgramMetrics::default(),
        ) {
            Ok(entry) => {
                if let ProgramCacheEntryType::Loaded(executable) = &entry.program {
                    let version = format!("{:?}", executable.get_sbpf_version());
                    *programs_by_version.entry(version).or_default() += 1;
                }
                seashell.accounts_db.programs.replenish(*address, Arc::new(entry));
                programs_loaded += 1;
            }
            Err(error) => programs_failed.push((*address, error.to_string())),
        }
    }

    let report = LoadReport {
        accounts: accounts.len(),
        active_features,
        known_features: FEATURE_NAMES.len(),
        programs_loaded,
        programs_by_version,
        programs_failed,
    };
    (seashell, report)
}

/// Every SBF program among `accounts` as `(address, SBPF version, loader, slot of the last
/// deployment)`. The slot is what the loader recorded: upgradeable programs keep it in the
/// program data account and loader v4 programs in the program account. The two older loaders
/// keep none, and their programs cannot be upgraded.
pub fn program_deployments(
    accounts: &HashMap<Pubkey, AccountSharedData>,
) -> Vec<(Pubkey, String, &'static str, Option<u64>)> {
    use solana_account::ReadableAccount;
    let slot_at = |data: &[u8], offset: usize| {
        data.get(offset..offset + 8).map(|slot| u64::from_le_bytes(slot.try_into().unwrap()))
    };
    let mut deployments = Vec::new();
    for (address, account) in accounts {
        if !account.executable() {
            continue;
        }
        let Ok(Some(elf)) = program_elf(account, accounts) else {
            continue;
        };
        let Ok(version) = solana_sbpf::elf::get_sbpf_version(elf) else {
            continue;
        };
        let owner = account.owner();
        let (loader, slot) = if *owner == bpf_loader_upgradeable::id() {
            // The program account is the tag and the address of the program data account, whose
            // data starts with its own tag and the deployment slot.
            let program_data = account.data().get(4..36).and_then(|address| {
                accounts.get(&Pubkey::from(<[u8; 32]>::try_from(address).unwrap()))
            });
            ("upgradeable", program_data.and_then(|account| slot_at(account.data(), 4)))
        } else if *owner == loader_v4::id() {
            ("v4", slot_at(account.data(), 0))
        } else if *owner == bpf_loader::id() {
            ("v2 (not upgradeable)", None)
        } else {
            ("v1 (not upgradeable)", None)
        };
        deployments.push((*address, format!("{version:?}"), loader, slot));
    }
    deployments
}

/// The ELF of the program in `account`, or `None` when the account is not owned by an SBF loader
/// (a builtin).
fn program_elf<'a>(
    account: &'a AccountSharedData,
    accounts: &'a HashMap<Pubkey, AccountSharedData>,
) -> Result<Option<&'a [u8]>, &'static str> {
    use solana_account::ReadableAccount;
    /// `UpgradeableLoaderState::Program` is the tag 2 and the address of the program data.
    const PROGRAM_TAG: [u8; 4] = [2, 0, 0, 0];
    /// `UpgradeableLoaderState::ProgramData` puts the ELF after tag, slot and optional authority.
    const PROGRAM_DATA_METADATA_LEN: usize = 45;
    /// `LoaderV4State` puts the ELF after slot, authority and status.
    const LOADER_V4_METADATA_LEN: usize = 48;

    let owner = account.owner();
    let data = account.data();
    if *owner == bpf_loader::id() || *owner == bpf_loader_deprecated::id() {
        Ok(Some(data))
    } else if *owner == bpf_loader_upgradeable::id() {
        let program_data_address: [u8; 32] = data
            .strip_prefix(&PROGRAM_TAG)
            .and_then(|address| address.try_into().ok())
            .ok_or("not an upgradeable program account")?;
        let program_data = accounts
            .get(&Pubkey::from(program_data_address))
            .ok_or("the program data account is not in the snapshot")?;
        program_data
            .data()
            .get(PROGRAM_DATA_METADATA_LEN..)
            .map(Some)
            .ok_or("the program data account is too short")
    } else if *owner == loader_v4::id() {
        data.get(LOADER_V4_METADATA_LEN..).map(Some).ok_or("the loader v4 account is too short")
    } else {
        Ok(None)
    }
}
