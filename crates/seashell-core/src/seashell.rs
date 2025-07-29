use std::cell::RefCell;
use std::rc::Rc;

use agave_feature_set::FeatureSet;
use solana_account::{Account, AccountSharedData, ReadableAccount, WritableAccount};
use solana_compute_budget::compute_budget::ComputeBudget;
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_instruction::error::InstructionError;
use solana_log_collector::LogCollector;
use solana_precompile_error::PrecompileError;
use solana_program_runtime::invoke_context::{EnvironmentConfig, InvokeContext};
use solana_pubkey::Pubkey;
use solana_svm_callback::InvokeContextCallback;
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
    pub feature_set: FeatureSet,
    pub log_collector: Option<Rc<RefCell<LogCollector>>>,
}

impl Default for Seashell {
    fn default() -> Self {
        Seashell {
            config: Config::default(),
            accounts_db: AccountsDb::default(),
            compute_budget: ComputeBudget::default(),
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
    // TODO:
    // set_account, load_program, etc. API
    // new_from_snapshot, etc. API
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

        let instruction_accounts = compile_accounts_for_instruction(&ixn);

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
                &instruction_accounts,
                &[INSTRUCTION_PROGRAM_ID_INDEX],
                std::iter::once(ixn.data.as_slice()),
            )
        } else {
            invoke_context.process_instruction(
                &ixn.data,
                &instruction_accounts,
                &[INSTRUCTION_PROGRAM_ID_INDEX],
                &mut compute_units_consumed,
                &mut ExecuteTimings::default(),
            )
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
                                let account = transaction_context
                                    .get_account_at_index(idx)
                                    .expect("Account should exist")
                                    .borrow()
                                    .to_owned();

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
                println!("Error processing ixn: {:?}", &e);
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
        let mut seashell = Seashell::new_with_config(Config { memoize: true });

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
        crate::set_log();
        let mut seashell = Seashell::new();

        // ed25519 precompile
        let secret_key = ed25519_dalek::Keypair::generate(&mut rand::thread_rng());
        let ixn = solana_ed25519_program::new_ed25519_instruction(&secret_key, b"test");

        let result = seashell.process_instruction(ixn);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.compute_units_consumed, 0);

        // secp256k1 precompile
        let secret_key = libsecp256k1::SecretKey::random(&mut rand::thread_rng());
        let ixn = solana_secp256k1_program::new_secp256k1_instruction(&secret_key, b"test");

        let result = seashell.process_instruction(ixn);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.compute_units_consumed, 0);

        // secp256r1 precompile
        let curve_name = openssl::nid::Nid::X9_62_PRIME256V1;
        let group = openssl::ec::EcGroup::from_curve_name(curve_name).unwrap();
        let secret_key = openssl::ec::EcKey::generate(&group).unwrap();
        let ixn = solana_secp256r1_program::new_secp256r1_instruction(b"test", secret_key).unwrap();

        let result = seashell.process_instruction(ixn);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.compute_units_consumed, 0);
    }
}
