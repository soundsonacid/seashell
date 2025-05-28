use std::collections::HashMap;

use solana_account::Account;
use solana_program_runtime::{loaded_programs::ProgramCacheForTxBatch, sysvar_cache::SysvarCache};
use solana_pubkey::Pubkey;

pub struct AccountsDb {
    pub overrides: HashMap<Pubkey, Account>,
    pub accounts: HashMap<Pubkey, Account>,
    pub programs: ProgramCacheForTxBatch,
    pub sysvars: SysvarCache,
}
