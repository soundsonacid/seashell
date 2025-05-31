use std::sync::Arc;

use solana_account::{Account, AccountSharedData, ReadableAccount, WritableAccount};
use solana_builtins::BUILTINS;
use solana_compute_budget::compute_budget::ComputeBudget;
use solana_feature_set::FeatureSet;
use solana_fee_structure::FeeStructure;
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_instruction::error::InstructionError;
use solana_program_runtime::invoke_context::{EnvironmentConfig, InvokeContext};
use solana_program_runtime::loaded_programs::ProgramCacheEntry;
use solana_pubkey::Pubkey;
use solana_timings::ExecuteTimings;
use solana_transaction_context::TransactionContext;

use crate::accounts_db::AccountsDb;
use crate::compile::{INSTRUCTION_PROGRAM_ID_INDEX, compile_accounts_for_instruction};

#[derive(Default)]
pub struct Config {
    pub memoize: bool,
}

pub struct Seashell {
    pub config: Config,
    pub accounts_db: AccountsDb,
    pub compute_budget: ComputeBudget,
    pub fee_structure: FeeStructure,
    pub feature_set: FeatureSet,
}

impl Seashell {
    // TODO:
    // load precompiles, spl programs
    // set_account, load_program, etc. API
    // new_from_snapshot, etc. API
    pub fn new() -> Self {
        #[rustfmt::skip]
        solana_logger::setup_with_default(
            "solana_rbpf::vm=debug,\
             solana_runtime::message_processor=debug,\
             solana_runtime::system_instruction_processor=trace",
        );

        let mut seashell = Seashell {
            config: Config::default(),
            accounts_db: AccountsDb::new(),
            compute_budget: ComputeBudget::default(),
            fee_structure: FeeStructure::default(),
            feature_set: FeatureSet::default(),
        };

        // TODO: revisit precision of this logic
        // do we need to set up processing environment?
        for builtin in BUILTINS {
            if builtin.enable_feature_id.is_none() {
                // register builtin..
                let builtin_program =
                    ProgramCacheEntry::new_builtin(0, builtin.name.len(), builtin.entrypoint);
                seashell
                    .accounts_db
                    .programs
                    .replenish(builtin.program_id, Arc::new(builtin_program));
                seashell.accounts_db.set_account_mock(builtin.program_id);
            }
        }

        seashell
    }

    pub fn process_instruction(&mut self, ixn: Instruction) -> InstructionProcessingResult {
        let transaction_accounts = self.accounts_db.accounts_for_instruction(&ixn);
        let sysvar_cache = self
            .accounts_db
            .sysvars_for_instruction(&transaction_accounts);
        let mut transaction_context = TransactionContext::new(
            transaction_accounts.clone(),
            self.accounts_db.sysvars.rent(),
            self.compute_budget.max_instruction_stack_depth,
            self.compute_budget.max_instruction_trace_length,
        );

        // get correct loader
        // process precompile vs regular program
        let instruction_accounts = compile_accounts_for_instruction(&ixn);

        let mut invoke_context = InvokeContext::new(
            &mut transaction_context,
            &mut self.accounts_db.programs,
            EnvironmentConfig::new(
                Hash::default(),
                0,
                0,
                &|_| 0,
                std::sync::Arc::new(self.feature_set.clone()),
                &sysvar_cache,
            ),
            None,
            self.compute_budget,
        );

        let mut compute_units_consumed = 0;
        let result = invoke_context.process_instruction(
            &ixn.data,
            &instruction_accounts,
            &[INSTRUCTION_PROGRAM_ID_INDEX],
            &mut compute_units_consumed,
            &mut ExecuteTimings::default(),
        );

        let return_data = transaction_context.get_return_data().1.to_owned();
        match result {
            Ok(_) => {
                println!(
                    "Instruction processed successfully, compute units consumed: {}",
                    compute_units_consumed
                );
                let resulting_accounts: Vec<(Pubkey, Account)> = transaction_accounts
                    .iter()
                    .map(|(pubkey, account_shared_data)| {
                        transaction_context
                            .find_index_of_account(&pubkey)
                            .map(|idx| {
                                let account: Account = transaction_context
                                    .get_account_at_index(idx)
                                    .expect("Account should exist")
                                    .borrow()
                                    .to_owned()
                                    .into();
                                (*pubkey, account)
                            })
                            .unwrap_or((*pubkey, account_shared_data.to_owned().into()))
                    })
                    .collect();
                InstructionProcessingResult {
                    compute_units_consumed,
                    return_data,
                    error: None,
                    resulting_accounts,
                }
            }
            Err(e) => {
                println!("Error processing ixn: {:?}", &e);
                InstructionProcessingResult {
                    compute_units_consumed,
                    return_data,
                    error: Some(InstructionProcessingError::InstructionError(e)),
                    resulting_accounts: vec![],
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
        self.accounts_db.set_account(pubkey, account);
    }
}

pub struct InstructionProcessingResult {
    pub compute_units_consumed: u64,
    pub return_data: Vec<u8>,
    pub error: Option<InstructionProcessingError>,
    pub resulting_accounts: Vec<(Pubkey, Account)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstructionProcessingError {
    InstructionError(InstructionError),
    ProgramError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_transfer() {
        use solana_instruction::AccountMeta;

        let mut seashell = Seashell::new();

        let from = solana_pubkey::Pubkey::new_unique();
        let to = solana_pubkey::Pubkey::new_unique();
        seashell.airdrop(from, 1000);
        seashell.accounts_db.set_account_mock(to);
        println!("Airdropped 1000 lamports to {}", from);

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
            .resulting_accounts
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
            .resulting_accounts
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
}
