use std::collections::HashMap;

use solana_account::{AccountSharedData, ReadableAccount};
use solana_instruction::Instruction;
use solana_program_runtime::loaded_programs::ProgramCacheForTxBatch;
use solana_program_runtime::sysvar_cache::SysvarCache;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionAccount;

use crate::sysvar::Sysvars;

pub fn mock_account_shared_data(pubkey: Pubkey) -> AccountSharedData {
    AccountSharedData::new(0, 0, &pubkey)
}

pub struct AccountsDb {
    pub overrides: HashMap<Pubkey, AccountSharedData>,
    pub accounts: HashMap<Pubkey, AccountSharedData>,
    pub programs: ProgramCacheForTxBatch,
    pub sysvars: Sysvars,
}

impl AccountsDb {
    pub fn new() -> Self {
        Self {
            overrides: HashMap::new(),
            accounts: HashMap::new(),
            programs: ProgramCacheForTxBatch::default(),
            sysvars: Sysvars::default(),
        }
    }

    // TODO: make sysvars part of self.accounts
    // TODO: use Account instead of AccountSharedData
    pub fn account_maybe(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        println!("Fetching account for pubkey: {}", pubkey);
        if self.sysvars.is_sysvar(pubkey) {
            return Some(self.sysvars.get(pubkey));
        }

        self.overrides
            .get(pubkey)
            .or_else(|| self.accounts.get(pubkey))
            .cloned()
    }

    pub fn account(&self, pubkey: &Pubkey) -> AccountSharedData {
        self.account_maybe(pubkey)
            .expect("Account should exist")
            .to_owned()
    }

    pub fn accounts_for_instruction(&self, instruction: &Instruction) -> Vec<TransactionAccount> {
        // always insert the program_id of the instruction as the first account.
        let mut accounts = vec![(instruction.program_id, self.account(&instruction.program_id))];
        instruction.accounts.iter().for_each(|meta| {
            let pubkey = meta.pubkey;
            accounts.push((pubkey, self.account(&pubkey)))
        });
        accounts
    }

    pub fn sysvars_for_instruction(&self, accounts: &[TransactionAccount]) -> SysvarCache {
        let mut sysvar_cache = SysvarCache::default();

        sysvar_cache.fill_missing_entries(|sysvar, set_sysvar| {
            if let Some(account) = accounts.iter().find(|(pubkey, _)| pubkey == sysvar) {
                // Check if the sysvar is in the instruction's accounts
                set_sysvar(account.1.data());
            } else {
                // If not, check our AccountsDb (which will always contain the sysvar as a fallback)
                // Optionality here is to avoid supporting Fees sysvar, which is deprecated but expected by SysvarCache
                let account = self.account_maybe(&sysvar);
                let data = account.map(|a| a.data().to_owned()).unwrap_or_default();
                set_sysvar(&data);
            }
        });

        sysvar_cache
    }

    pub fn set_accounts(&mut self, updates: Vec<(Pubkey, AccountSharedData)>) {
        updates.into_iter().for_each(|(pubkey, account)| {
            self.set_account(pubkey, account);
        });
    }

    pub fn set_account(&mut self, pubkey: Pubkey, account: AccountSharedData) {
        self.accounts.insert(pubkey, account);
    }

    pub fn set_account_mock(&mut self, pubkey: Pubkey) {
        let account = mock_account_shared_data(pubkey);
        self.set_account(pubkey, account);
    }
}
