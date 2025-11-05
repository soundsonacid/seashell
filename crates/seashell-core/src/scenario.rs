use std::cell::Cell;
use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use solana_account::{Account, AccountSharedData};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;

/// Scenario manages account overrides with automatic persistence.
/// It stores accounts as AccountSharedData internally but serializes as Account.
/// When an RPC client is provided, missing accounts are fetched and persisted.
#[derive(Default)]
pub struct Scenario {
    should_persist: Cell<bool>,
    dirty: Cell<bool>,
    data: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
    path: Option<PathBuf>,
    rpc_client: Option<RpcClient>,
}

#[serde_as]
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
struct SerializableScenario(
    #[serde_as(as = "HashMap<serde_with::DisplayFromStr, AccountAsJsonAccount>")]
    HashMap<Pubkey, Account>,
);

#[serde_as]
#[derive(Serialize, Deserialize)]
struct JsonAccount {
    #[serde(default)]
    pub lamports: u64,
    #[serde_as(as = "serde_with::hex::Hex")]
    pub data: Vec<u8>,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub owner: Pubkey,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub rent_epoch: u64,
}

impl From<JsonAccount> for Account {
    fn from(value: JsonAccount) -> Self {
        Account {
            lamports: value.lamports,
            data: value.data,
            owner: value.owner,
            executable: value.executable,
            rent_epoch: value.rent_epoch,
        }
    }
}

impl From<Account> for JsonAccount {
    fn from(value: Account) -> Self {
        JsonAccount {
            lamports: value.lamports,
            data: value.data,
            owner: value.owner,
            executable: value.executable,
            rent_epoch: value.rent_epoch,
        }
    }
}

serde_with::serde_conv!(
    AccountAsJsonAccount,
    Account,
    |account: &Account| { JsonAccount::from(account.clone()) },
    |account: JsonAccount| -> Result<_, std::convert::Infallible> { Ok(account.into()) }
);

impl Scenario {
    /// Load a scenario from a file, or create an empty one if the file doesn't exist.
    pub fn from_file(path: PathBuf) -> Self {
        let data = if path.exists() {
            let serializable: SerializableScenario = read_json_gz(&path);
            serializable
                .0
                .into_iter()
                .map(|(pubkey, account)| (pubkey, account.into()))
                .collect()
        } else {
            HashMap::new()
        };

        Scenario {
            should_persist: Cell::new(true),
            dirty: Cell::new(false),
            data: Arc::new(RwLock::new(data)),
            path: Some(path),
            rpc_client: None,
        }
    }

    /// Load a scenario with RPC fallback enabled.
    pub fn from_file_with_rpc(path: PathBuf, rpc_url: String) -> Self {
        let mut scenario = Self::from_file(path);
        scenario.rpc_client = Some(RpcClient::new(rpc_url));
        scenario
    }

    pub fn rpc_only(rpc_url: String) -> Self {
        Scenario {
            should_persist: Cell::new(false),
            dirty: Cell::new(false),
            data: Arc::new(RwLock::new(HashMap::new())),
            path: None,
            rpc_client: Some(RpcClient::new(rpc_url)),
        }
    }

    /// Fetch an account from RPC and store it in the scenario.
    /// Panics if RPC is not configured or if the RPC request fails.
    pub fn fetch_from_rpc(&self, pubkey: &Pubkey) -> AccountSharedData {
        log::debug!("Attempting to fetch account: {pubkey}");
        let rpc_client = self.rpc_client.as_ref().expect(
            "Account not found in scenario or accounts. RPC URL must be configured to fetch \
             missing accounts.",
        );

        let failure_msg = format!("Failed to fetch account {pubkey} from RPC");
        let account = rpc_client.get_account(pubkey).expect(&failure_msg);

        let account_shared: AccountSharedData = account.into();
        self.dirty.set(true);
        self.data.write().insert(*pubkey, account_shared.clone());
        account_shared
    }

    pub fn get(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.data.read().get(pubkey).cloned()
    }

    pub fn insert(&mut self, pubkey: Pubkey, account: AccountSharedData) {
        self.dirty.set(true);
        self.data.write().insert(pubkey, account);
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        if self.dirty.get() && self.should_persist.get() {
            if let Some(path) = &self.path {
                // Convert AccountSharedData back to Account for serialization
                let accounts: HashMap<Pubkey, Account> = self
                    .data
                    .read()
                    .iter()
                    .map(|(pubkey, account_shared)| (*pubkey, account_shared.clone().into()))
                    .collect();

                let serializable = SerializableScenario(accounts);

                // Ensure the parent directory exists
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }

                try_write_json_gz(path, &serializable);
            }
        }
    }
}

pub fn try_write_json_gz<T>(path: &Path, data: &T)
where
    T: Serialize,
{
    let file = match std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(path)
    {
        Ok(file) => file,
        Err(err) => {
            eprintln!("Failed to write to file; path={path:?}; err={err}");
            return;
        }
    };
    let compression = GzEncoder::new(file, flate2::Compression::best());

    match serde_json::to_writer(compression, &data) {
        Ok(serialized) => serialized,
        Err(err) => {
            eprintln!("Failed to serialize data; path={path:?}; err={err}");
        }
    }
}

pub fn read_json_gz<T>(path: &Path) -> T
where
    T: DeserializeOwned,
{
    let compressed = open_read(path);
    let bytes = BufReader::new(GzDecoder::new(compressed));

    serde_json::from_reader(bytes).unwrap()
}

fn open_read(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .unwrap_or_else(|err| panic!("Failed to open file; path={path:?}; err={err}"))
}
