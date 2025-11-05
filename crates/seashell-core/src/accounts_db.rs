use std::collections::HashMap;
use std::sync::Arc;

use agave_feature_set::FeatureSet;
use agave_syscalls::create_program_runtime_environment_v1;
use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_compute_budget::compute_budget::ComputeBudget;
use solana_instruction::Instruction;
use solana_program_runtime::loaded_programs::{
    LoadProgramMetrics, ProgramCacheEntry, ProgramCacheForTxBatch,
};
use solana_program_runtime::sysvar_cache::SysvarCache;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionAccount;

use crate::scenario::Scenario;
use crate::sysvar::Sysvars;

pub fn mock_account_shared_data(pubkey: Pubkey) -> AccountSharedData {
    AccountSharedData::new(0, 0, &pubkey)
}

#[derive(Default)]
pub struct AccountsDb {
    pub scenario: Scenario,
    pub accounts: HashMap<Pubkey, AccountSharedData>,
    pub programs: ProgramCacheForTxBatch,
    pub sysvars: Sysvars,
}

impl AccountsDb {
    pub fn account_maybe(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        if self.sysvars.is_sysvar(pubkey) {
            return Some(self.sysvars.get(pubkey));
        }

        // 1. Check scenario overrides
        if let Some(account) = self.scenario.get(pubkey) {
            return Some(account.clone());
        }

        // 2. Check regular accounts
        if let Some(account) = self.accounts.get(pubkey) {
            return Some(account.clone());
        }

        None
    }

    pub fn account(&self, pubkey: &Pubkey) -> AccountSharedData {
        self.account_maybe(pubkey)
            .unwrap_or_else(|| self.scenario.fetch_from_rpc(pubkey))
    }

    pub fn accounts_for_instruction(
        &mut self,
        allow_uninitialized_accounts: bool,
        instruction: &Instruction,
    ) -> Vec<TransactionAccount> {
        // always insert the program_id of the instruction as the first account.
        let mut accounts = vec![(instruction.program_id, self.account(&instruction.program_id))];
        instruction.accounts.iter().for_each(|meta| {
            let pubkey = meta.pubkey;
            if allow_uninitialized_accounts {
                let account = self.account_maybe(&pubkey).unwrap_or_else(|| {
                    log::debug!("Creating uninitialized account for {pubkey}");
                    AccountSharedData::default()
                });
                accounts.push((pubkey, account))
            } else {
                accounts.push((pubkey, self.account(&pubkey)))
            }
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
                let account = self.account_maybe(sysvar);
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
        if self.sysvars.is_sysvar(&pubkey) {
            self.sysvars.set(&pubkey, account)
        } else {
            self.accounts.insert(pubkey, account);
        }
    }

    pub fn set_account_mock(&mut self, pubkey: Pubkey) {
        let account = mock_account_shared_data(pubkey);
        self.set_account(pubkey, account);
    }

    // TODO: revisit precision of this logic
    // do we need to set up processing environment?
    pub fn load_builtins(&mut self, feature_set: &FeatureSet) {
        for builtin in solana_builtins::BUILTINS {
            if builtin
                .enable_feature_id
                .is_none_or(|feature_id| feature_set.is_active(&feature_id))
            {
                let builtin_program =
                    ProgramCacheEntry::new_builtin(0, builtin.name.len(), builtin.entrypoint);
                self.programs
                    .replenish(builtin.program_id, Arc::new(builtin_program));
                let mut account_shared_data =
                    AccountSharedData::new(1, 0, &solana_sdk_ids::native_loader::id());
                account_shared_data.set_executable(true);
                self.set_account(builtin.program_id, account_shared_data);
            }
        }
    }

    pub fn load_program_from_bytes_with_loader(
        &mut self,
        program_id: Pubkey,
        bytes: &[u8],
        loader: Pubkey,
        feature_set: &FeatureSet,
        compute_budget: &ComputeBudget,
    ) {
        let current_slot = self.sysvars.clock().slot;
        let account_size = bytes.len();
        let minimum_balance_for_rent_exemption = self.sysvars.rent().minimum_balance(account_size);
        let mut program_account_shared_data =
            AccountSharedData::new(minimum_balance_for_rent_exemption, account_size, &loader);
        program_account_shared_data.set_executable(true);
        let program_runtime_environment = Arc::new(
            create_program_runtime_environment_v1(
                &feature_set.runtime_features(),
                &compute_budget.to_budget(),
                false,
                false,
            )
            .expect("Failed to create program runtime environment"),
        );
        let program_cache_entry = ProgramCacheEntry::new(
            &loader,
            program_runtime_environment,
            current_slot,
            current_slot,
            bytes,
            account_size,
            &mut LoadProgramMetrics::default(),
        )
        .expect(&format!("Failed to load program {program_id} from bytes"));
        self.set_account(program_id, program_account_shared_data);
        self.programs
            .replenish(program_id, Arc::new(program_cache_entry));
    }
}
