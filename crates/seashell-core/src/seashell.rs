use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use agave_feature_set::FeatureSet;
use solana_account::{Account, AccountSharedData, ReadableAccount, WritableAccount};
use solana_compute_budget::compute_budget::ComputeBudget;
use solana_hash::Hash;
use solana_instruction::error::InstructionError;
use solana_instruction::Instruction;
use solana_precompile_error::PrecompileError;
use solana_program_runtime::invoke_context::{EnvironmentConfig, InvokeContext};
use solana_pubkey::Pubkey;
use solana_svm_callback::InvokeContextCallback;
use solana_svm_log_collector::LogCollector;
use solana_svm_timings::ExecuteTimings;
use solana_transaction_context::{IndexOfAccount, TransactionContext};

use crate::accounts_db::AccountsDb;
use crate::compile::{compile_accounts_for_instruction, INSTRUCTION_PROGRAM_ID_INDEX};
use crate::error::SeashellError;
use crate::scenario::Scenario;

pub struct Config {
    pub memoize: bool,
    pub allow_uninitialized_accounts: bool,
}

// Allow deriving Default manually to be explicit about configuration defaults
#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Config { memoize: false, allow_uninitialized_accounts: false }
    }
}

pub struct Seashell {
    pub config: Config,
    pub accounts_db: AccountsDb,
    pub compute_budget: ComputeBudget,
    pub feature_set: FeatureSet,
    pub log_collector: Option<Rc<RefCell<LogCollector>>>,
}

unsafe impl Send for Seashell {}
unsafe impl Sync for Seashell {}

impl Default for Seashell {
    fn default() -> Self {
        Seashell {
            config: Config::default(),
            accounts_db: AccountsDb::default(),
            compute_budget: ComputeBudget::new_with_defaults(false),
            feature_set: FeatureSet::all_enabled(),
            log_collector: None,
        }
    }
}
struct SeashellInvokeContextCallback<'a> {
    feature_set: &'a FeatureSet,
}

impl InvokeContextCallback for SeashellInvokeContextCallback<'_> {
    fn is_precompile(&self, program_id: &Pubkey) -> bool {
        agave_precompiles::is_precompile(program_id, |feature| self.feature_set.is_active(feature))
    }

    fn process_precompile(
        &self,
        program_id: &Pubkey,
        data: &[u8],
        instruction_datas: Vec<&[u8]>,
    ) -> Result<(), PrecompileError> {
        if let Some(precompile) = agave_precompiles::get_precompile(program_id, |feature_id| {
            self.feature_set.is_active(feature_id)
        }) {
            precompile.verify(data, &instruction_datas, self.feature_set)
        } else {
            Err(PrecompileError::InvalidPublicKey)
        }
    }
}

impl Seashell {
    pub fn new() -> Self {
        #[rustfmt::skip]
        solana_logger::setup_with_default(
            "solana_rbpf::vm=debug,\
             solana_runtime::message_processor=debug,\
             solana_runtime::system_instruction_processor=trace",
        );

        let mut seashell = Seashell::default();

        seashell.accounts_db.load_builtins(&seashell.feature_set);

        seashell.load_spl();
        seashell.load_precompiles();

        seashell
    }

    pub fn new_with_config(config: Config) -> Self {
        let mut seashell = Seashell::new();
        seashell.config = config;
        seashell
    }

    pub fn enable_log_collector(&mut self) {
        self.log_collector = Some(Rc::new(RefCell::new(LogCollector::default())))
    }

    pub fn logs(&self) -> Option<Vec<String>> {
        self.log_collector
            .as_ref()
            .map(|log_collector| log_collector.borrow().get_recorded_content().to_owned())
    }

    pub fn load_spl(&mut self) {
        crate::spl::load(self);
    }

    pub fn load_precompiles(&mut self) {
        crate::precompiles::load(self);
    }

    pub fn load_program_from_bytes(&mut self, program_id: Pubkey, bytes: &[u8]) {
        self.accounts_db.load_program_from_bytes_with_loader(
            program_id,
            bytes,
            solana_sdk_ids::bpf_loader::id(),
            &self.feature_set,
            &self.compute_budget,
        );
    }

    /// Attempts to locate a program `.so` in the workspace root `target/deploy` directory or the `SBF_OUT_DIR` named `<program_name>.so`.
    pub fn load_program_from_environment(
        &mut self,
        program_name: &str,
        program_id: Pubkey,
    ) -> Result<(), SeashellError> {
        let program_so_directory = if let Ok(out_dir) = std::env::var("SBF_OUT_DIR") {
            // First try to read from the SBF_OUT_DIR environment variable
            PathBuf::from(out_dir)
        } else {
            // If not present, attempt to locate the workspace root
            let workspace_root = try_find_workspace_root()
                .ok_or(SeashellError::Custom("Could not locate workspace root".to_string()))?;
            workspace_root.join("target/deploy")
        };

        let entries = std::fs::read_dir(program_so_directory)?;

        for entry_maybe in entries {
            let entry = entry_maybe?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "so")
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|stem| stem == program_name)
            {
                let program_bytes = std::fs::read(path)?;
                self.accounts_db.load_program_from_bytes_with_loader(
                    program_id,
                    &program_bytes,
                    solana_sdk_ids::bpf_loader::id(),
                    &self.feature_set,
                    &self.compute_budget,
                );
            }
        }

        Ok(())
    }

    /// Loads a scenario from a .json.gz file, or creates a new empty scenario if the file doesn't exist.
    ///
    /// The scenario file should be in the "scenarios" directory of the current crate.
    /// Accounts from the scenario will override any existing accounts.
    /// When the scenario is dropped, it will be written back to the file.
    ///
    /// If the RPC URL environment variable is set, missing accounts will be fetched from the RPC.
    pub fn load_scenario(&mut self, scenario_name: &str) {
        let workspace_root = try_find_workspace_root().expect("Failed to locate workspace root");
        let scenario_path = workspace_root.join(format!("scenarios/{scenario_name}.json.gz"));

        self.accounts_db.scenario = if let Ok(ref rpc_url) = std::env::var("RPC_URL") {
            Scenario::from_file_with_rpc(scenario_path, rpc_url.clone())
        } else {
            Scenario::from_file(scenario_path)
        };
    }

    pub fn process_instruction(&mut self, ixn: Instruction) -> InstructionProcessingResult {
        let transaction_accounts = self
            .accounts_db
            .accounts_for_instruction(self.config.allow_uninitialized_accounts, &ixn);

        let sysvar_cache = self
            .accounts_db
            .sysvars_for_instruction(&transaction_accounts);
        let mut transaction_context = TransactionContext::new(
            transaction_accounts.clone(),
            self.accounts_db.sysvars.rent(),
            self.compute_budget.max_instruction_stack_depth,
            self.compute_budget.max_instruction_trace_length,
        );

        let instruction_accounts = compile_accounts_for_instruction(&ixn);

        let mut dedup_map = vec![u8::MAX; solana_transaction_context::MAX_ACCOUNTS_PER_TRANSACTION];
        for (idx, account) in instruction_accounts.iter().enumerate() {
            let index_in_instruction = dedup_map
                .get_mut(account.index_in_transaction as usize)
                .unwrap();
            if *index_in_instruction == u8::MAX {
                *index_in_instruction = idx as u8;
            }
        }

        transaction_context
            .configure_next_instruction(
                INSTRUCTION_PROGRAM_ID_INDEX as IndexOfAccount,
                instruction_accounts,
                dedup_map,
                &ixn.data,
            )
            .expect("Failed to configure instruction");

        let epoch_stake_callback = SeashellInvokeContextCallback { feature_set: &self.feature_set };
        let runtime_features = self.feature_set.runtime_features();
        let mut invoke_context = InvokeContext::new(
            &mut transaction_context,
            &mut self.accounts_db.programs,
            EnvironmentConfig::new(
                Hash::default(),
                /* blockhash_lamports_per_signature */ 5000, // The default value
                &epoch_stake_callback,
                &runtime_features,
                &sysvar_cache,
            ),
            self.log_collector.clone(),
            self.compute_budget.to_budget(),
            self.compute_budget.to_cost(),
        );

        let mut compute_units_consumed = 0;

        let result = if invoke_context.is_precompile(&ixn.program_id) {
            invoke_context.process_precompile(
                &ixn.program_id,
                &ixn.data,
                std::iter::once(ixn.data.as_slice()),
            )
        } else {
            invoke_context
                .process_instruction(&mut compute_units_consumed, &mut ExecuteTimings::default())
        };

        let return_data = transaction_context.get_return_data().1.to_owned();
        match result {
            Ok(_) => {
                let post_execution_accounts: Vec<(Pubkey, Account)> = transaction_accounts
                    .iter()
                    .map(|(pubkey, account_shared_data)| {
                        transaction_context
                            .find_index_of_account(pubkey)
                            .map(|idx| {
                                let accounts = transaction_context.accounts();
                                let account = accounts
                                    .try_borrow(idx)
                                    .expect("Failed to borrow TransactionAccounts")
                                    .clone();
                                if self.config.memoize {
                                    self.set_account_from_account_shared_data(
                                        *pubkey,
                                        account.clone(),
                                    );
                                }

                                (*pubkey, account.into())
                            })
                            .unwrap_or((*pubkey, account_shared_data.to_owned().into()))
                    })
                    .collect();

                InstructionProcessingResult {
                    compute_units_consumed,
                    return_data,
                    error: None,
                    post_execution_accounts,
                }
            }
            Err(e) => {
                eprintln!("Error processing ixn: {:?}", &e);
                InstructionProcessingResult {
                    compute_units_consumed,
                    return_data,
                    error: Some(InstructionProcessingError::InstructionError(e)),
                    post_execution_accounts: Vec::default(),
                }
            }
        }
    }

    pub fn airdrop(&mut self, pubkey: Pubkey, amount: u64) {
        let mut account = self
            .accounts_db
            .account_maybe(&pubkey)
            .unwrap_or_else(|| AccountSharedData::new(0, 0, &solana_sdk_ids::system_program::id()));
        account.set_lamports(account.lamports() + amount);
        self.set_account_from_account_shared_data(pubkey, account);
    }

    pub fn account(&self, pubkey: &Pubkey) -> Account {
        self.accounts_db.account(pubkey).into()
    }

    pub fn set_account(&mut self, pubkey: Pubkey, account: Account) {
        self.accounts_db.set_account(pubkey, account.into());
    }

    pub fn set_account_from_account_shared_data(
        &mut self,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) {
        self.accounts_db.set_account(pubkey, account);
    }
}

pub struct InstructionProcessingResult {
    pub compute_units_consumed: u64,
    pub return_data: Vec<u8>,
    pub error: Option<InstructionProcessingError>,
    pub post_execution_accounts: Vec<(Pubkey, Account)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstructionProcessingError {
    InstructionError(InstructionError),
    ProgramError,
}

pub fn try_find_workspace_root() -> Option<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or("cargo".to_owned());
    let output = std::process::Command::new(cargo)
        .arg("locate-project")
        .arg("--workspace")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let root = parsed["root"]
        .as_str()
        .unwrap()
        .strip_suffix("Cargo.toml")?;

    Some(PathBuf::from(root))
}

#[cfg(test)]
mod tests {
    use solana_instruction::AccountMeta;

    use super::*;

    fn create_mint_account(seashell: &mut Seashell, pubkey: Pubkey, amount: u64) {
        const MINT_ACCOUNT_SIZE: usize = 82;
        const MINT_ACCOUNT_RENT_EXEMPTION: u64 = 1461600;
        let mut account = AccountSharedData::new(
            MINT_ACCOUNT_RENT_EXEMPTION,
            MINT_ACCOUNT_SIZE,
            &solana_sdk_ids::system_program::id(),
        );
        account.set_owner(crate::spl::TOKEN_PROGRAM_ID);
        let mut data = vec![0; MINT_ACCOUNT_SIZE];
        data[36..44].copy_from_slice(&amount.to_le_bytes());
        account.set_data_from_slice(&data);
        account.set_lamports(1000);
        seashell.accounts_db.set_account(pubkey, account.clone());
    }

    fn create_token_account(
        seashell: &mut Seashell,
        pubkey: Pubkey,
        mint: Pubkey,
        owner: Pubkey,
        amount: u64,
    ) {
        const TOKEN_ACCOUNT_SIZE: usize = 165;
        const TOKEN_ACCOUNT_RENT_EXEMPTION: u64 = 2039000;
        let mut account = AccountSharedData::new(
            TOKEN_ACCOUNT_RENT_EXEMPTION,
            TOKEN_ACCOUNT_SIZE,
            &solana_sdk_ids::system_program::id(),
        );
        account.set_owner(crate::spl::TOKEN_PROGRAM_ID);
        let mut data = vec![0; TOKEN_ACCOUNT_SIZE];
        data[0..32].copy_from_slice(&mint.to_bytes());
        data[32..64].copy_from_slice(&owner.to_bytes());
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        data[108] = 1; // `AccountState::Initialized` state
        account.set_data_from_slice(&data);
        account.set_lamports(1000);
        account.set_owner(crate::spl::TOKEN_PROGRAM_ID);
        seashell.accounts_db.set_account(pubkey, account.clone());
    }

    #[test]
    fn test_native_transfer() {
        crate::set_log();
        let mut seashell = Seashell::new();

        let from = solana_pubkey::Pubkey::new_unique();
        let to = solana_pubkey::Pubkey::new_unique();
        seashell.airdrop(from, 1000);
        seashell.accounts_db.set_account_mock(to);
        println!("Airdropped 1000 lamports to {from}");

        let mut data = Vec::with_capacity(12);
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&500u64.to_le_bytes());

        let ixn = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(from, true), AccountMeta::new(to, false)],
            data,
        };

        let result = seashell.process_instruction(ixn);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.compute_units_consumed, 150);

        let post_from = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == from)
            .expect("Resulting account should exist")
            .to_owned()
            .1;
        assert_eq!(
            post_from.lamports(),
            500,
            "Expected from account to have 500 lamports after transfer"
        );

        let post_to = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == to)
            .expect("Resulting account should exist")
            .to_owned()
            .1;
        assert_eq!(
            post_to.lamports(),
            500,
            "Expected to account to have 500 lamports after transfer"
        );

        assert!(
            result.return_data.is_empty(),
            "Expected no return data, got: {:?}",
            result.return_data
        );
    }

    #[test]
    fn test_spl_transfer() {
        crate::set_log();
        let mut seashell = Seashell::new();
        let from: Pubkey = solana_pubkey::Pubkey::new_unique();
        let to = solana_pubkey::Pubkey::new_unique();
        let from_authority = solana_pubkey::Pubkey::new_unique();
        let mint = solana_pubkey::Pubkey::new_unique();

        create_mint_account(&mut seashell, mint, 1000);
        create_token_account(&mut seashell, from, mint, from_authority, 1000);
        create_token_account(&mut seashell, to, mint, Pubkey::new_unique(), 0);
        seashell.airdrop(from_authority, 1000);

        let mut data = [0; 9];
        data[0] = 3;
        data[1..9].copy_from_slice(&500u64.to_le_bytes());

        let ixn = Instruction {
            program_id: crate::spl::TOKEN_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(from, true),
                AccountMeta::new(to, false),
                AccountMeta::new_readonly(from_authority, true),
            ],
            data: data.to_vec(),
        };

        let result = seashell.process_instruction(ixn);

        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.compute_units_consumed, 4644);

        let post_from = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == from)
            .expect("Resulting account should exist")
            .to_owned()
            .1;
        let post_from_balance = u64::from_le_bytes(post_from.data[64..72].try_into().unwrap());
        assert_eq!(
            post_from_balance, 500,
            "Expected from token account to have 500 tokens after transfer"
        );

        let post_to = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == to)
            .expect("Resulting account should exist")
            .to_owned()
            .1;
        let post_to_balance = u64::from_le_bytes(post_to.data[64..72].try_into().unwrap());
        assert_eq!(
            post_to_balance, 500,
            "Expected to token account to have 500 tokens after transfer"
        );

        assert!(
            result.return_data.is_empty(),
            "Expected no return data, got: {:?}",
            result.return_data
        );
    }

    #[test]
    fn test_memoize() {
        crate::set_log();
        let mut seashell = Seashell::new_with_config(Config {
            memoize: true,
            allow_uninitialized_accounts: false,
        });

        let from = solana_pubkey::Pubkey::new_unique();
        let to = solana_pubkey::Pubkey::new_unique();
        seashell.airdrop(from, 1000);
        seashell.accounts_db.set_account_mock(to);
        println!("Airdropped 1000 lamports to {from}");

        let mut data = Vec::with_capacity(12);
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&500u64.to_le_bytes());

        let ixn = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(from, true), AccountMeta::new(to, false)],
            data,
        };

        let result = seashell.process_instruction(ixn);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.compute_units_consumed, 150);

        let post_from = seashell.account(&from);
        assert_eq!(
            post_from.lamports(),
            500,
            "Expected from account to have 500 lamports after transfer"
        );
        let post_to = seashell.account(&to);
        assert_eq!(
            post_to.lamports(),
            500,
            "Expected to account to have 500 lamports after transfer"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn test_precompiles() {
        const MESSAGE_LENGTH: usize = 128;
        crate::set_log();
        let mut seashell = Seashell::new();

        use rand::{thread_rng, Rng};
        let mut rng = thread_rng();

        // ed25519 precompile
        use ed25519_dalek::Signer;
        let privkey = ed25519_dalek::Keypair::generate(&mut rng);
        let message: Vec<u8> = (0..MESSAGE_LENGTH).map(|_| rng.gen_range(0, 255)).collect();
        let signature = privkey.sign(&message).to_bytes();
        let pubkey = privkey.public.to_bytes();
        let ixn = solana_ed25519_program::new_ed25519_instruction_with_signature(
            &message, &signature, &pubkey,
        );

        let result = seashell.process_instruction(ixn);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.compute_units_consumed, 0);

        // secp256k1 precompile
        let secp_privkey = libsecp256k1::SecretKey::random(&mut thread_rng());
        let message: Vec<u8> = (0..MESSAGE_LENGTH).map(|_| rng.gen_range(0, 255)).collect();
        let secp_pubkey = libsecp256k1::PublicKey::from_secret_key(&secp_privkey);
        let eth_address = solana_secp256k1_program::eth_address_from_pubkey(
            &secp_pubkey.serialize()[1..].try_into().unwrap(),
        );
        let (signature, recovery_id) =
            solana_secp256k1_program::sign_message(&secp_privkey.serialize(), &message).unwrap();
        let ixn = solana_secp256k1_program::new_secp256k1_instruction_with_signature(
            &message,
            &signature,
            recovery_id,
            &eth_address,
        );

        let result = seashell.process_instruction(ixn);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.compute_units_consumed, 0);

        // secp256r1 precompile
        use openssl::bn::BigNumContext;
        use openssl::ec::{EcGroup, EcKey};
        use openssl::nid::Nid;
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let secp_privkey = EcKey::generate(&group).unwrap();
        let message: Vec<u8> = (0..MESSAGE_LENGTH).map(|_| rng.gen_range(0, 255)).collect();
        let signature = solana_secp256r1_program::sign_message(
            &message,
            &secp_privkey.private_key_to_der().unwrap(),
        )
        .unwrap();
        let mut ctx = BigNumContext::new().unwrap();
        let pubkey = secp_privkey
            .public_key()
            .to_bytes(&group, openssl::ec::PointConversionForm::COMPRESSED, &mut ctx)
            .unwrap();
        let ixn = solana_secp256r1_program::new_secp256r1_instruction_with_signature(
            &message,
            &signature,
            &pubkey.try_into().unwrap(),
        );
        let result = seashell.process_instruction(ixn);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.compute_units_consumed, 0);
    }

    #[test]
    fn test_load_from_environment() {
        crate::set_log();
        let mut seashell = Seashell::new();
        let spl_elfs_out_dir = try_find_workspace_root()
            .unwrap()
            .join("crates/seashell-core/src/spl/elfs");
        unsafe { std::env::set_var("SBF_OUT_DIR", spl_elfs_out_dir.to_str().unwrap()) }

        let tokenkeg = Pubkey::new_unique();
        seashell
            .load_program_from_environment("tokenkeg", tokenkeg)
            .unwrap();

        let token22 = Pubkey::new_unique();
        seashell
            .load_program_from_environment("token22", token22)
            .unwrap();

        let associated_token = Pubkey::new_unique();
        seashell
            .load_program_from_environment("associated_token", associated_token)
            .unwrap();

        assert!(seashell.accounts_db.accounts.contains_key(&tokenkeg));
        assert!(seashell.accounts_db.accounts.contains_key(&token22));
        assert!(seashell
            .accounts_db
            .accounts
            .contains_key(&associated_token));
    }

    #[test]
    fn test_scenario_loading() {
        use std::fs;

        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let scenarios_dir = temp_dir.path().join("scenarios");
        fs::create_dir_all(&scenarios_dir).unwrap();

        let mut seashell = Seashell::new_with_config(Config {
            memoize: false,
            allow_uninitialized_accounts: false,
        });

        let pubkey1 = Pubkey::from_str_const("B91piBSfCBRs5rUxCMRdJEGv7tNEnFxweWcdQJHJoFpi");
        let pubkey2 = Pubkey::from_str_const("6gAnjderE13TGGFeqdPVQ438jp2FPVeyXAszxKu9y338");

        // Load scenario (should create new file)
        unsafe { std::env::set_var("RPC_URL", "https://api.mainnet-beta.solana.com") };
        seashell.load_scenario("test_scenario");

        // Verify accounts are currently accessible
        // Will panic if not set
        seashell.account(&pubkey1);
        seashell.account(&pubkey2);

        // Drop seashell to trigger scenario save
        drop(seashell);

        // Create new seashell and load the saved scenario
        let mut seashell2 = Seashell::new();
        seashell2.load_scenario("test_scenario");

        // Verify accounts were persisted and loaded
        // Will panic if not set
        seashell2.account(&pubkey1);
        seashell2.account(&pubkey2);

        unsafe { std::env::remove_var("RPC_URL") }
    }

    #[test]
    fn test_account_lookup_order() {
        let mut seashell = Seashell::new();

        let pubkey = Pubkey::new_unique();

        seashell.airdrop(pubkey, 1000);
        assert_eq!(seashell.account(&pubkey).lamports(), 1000);

        use std::fs;

        use tempfile::TempDir;
        let temp_dir = TempDir::new().unwrap();
        let scenarios_dir = temp_dir.path().join("scenarios");
        fs::create_dir_all(&scenarios_dir).unwrap();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) }

        seashell.load_scenario("test_override");

        let override_account =
            AccountSharedData::new(2000, 0, &solana_sdk_ids::system_program::id());
        seashell
            .accounts_db
            .scenario
            .insert(pubkey, override_account);

        assert_eq!(seashell.account(&pubkey).lamports(), 2000);
    }

    #[test]
    #[should_panic(expected = "Account not found in scenario or accounts. RPC URL must be \
                               configured to fetch missing accounts.")]
    fn test_missing_account_without_rpc() {
        let mut seashell = Seashell::new();

        use std::fs;

        use tempfile::TempDir;
        let temp_dir = TempDir::new().unwrap();
        let scenarios_dir = temp_dir.path().join("scenarios");
        fs::create_dir_all(&scenarios_dir).unwrap();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) }

        // Ensure RPC_URL is not set
        unsafe { std::env::remove_var("RPC_URL") }
        seashell.load_scenario("test_no_rpc");

        let missing_pubkey = Pubkey::from_str_const("NoShot1111111111111111111111111111111111111");
        seashell.account(&missing_pubkey);
    }
}
