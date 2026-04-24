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
use solana_program_runtime::loaded_programs::ProgramRuntimeEnvironments;
use solana_pubkey::Pubkey;
use solana_svm_callback::InvokeContextCallback;
use solana_svm_log_collector::LogCollector;
use solana_svm_timings::ExecuteTimings;
use solana_transaction_context::{IndexOfAccount, TransactionContext};

use crate::accounts_db::AccountsDb;
use crate::compile::{
    compile_accounts_for_instruction, compile_instruction_for_transaction,
    compile_transaction_account_keys, INSTRUCTION_PROGRAM_ID_INDEX,
};
use crate::error::SeashellError;
use crate::scenario::Scenario;
use crate::sysvar::SysvarInstructions;

pub struct Config {
    pub memoize: bool,
    pub allow_uninitialized_accounts_local: bool,
    pub allow_uninitialized_accounts_fetched: bool,
}

// Allow deriving Default manually to be explicit about configuration defaults
#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Config {
            memoize: false,
            allow_uninitialized_accounts_local: false,
            allow_uninitialized_accounts_fetched: false,
        }
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
            compute_budget: ComputeBudget::new_with_defaults(false, false),
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

    /// Replaces the Tokenkeg binary with the P-Token binary.
    pub fn use_p_token(&mut self) {
        crate::spl::load_p_token(self);
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
    /// If `rpc_url` is provided, missing accounts will be fetched from that RPC endpoint.
    /// If `rpc_url` is `None`, falls back to the `RPC_URL` environment variable.
    pub fn load_scenario(&mut self, scenario_name: &str, rpc_url: Option<&str>) {
        let workspace_root = try_find_workspace_root().expect("Failed to locate workspace root");
        let scenario_path = workspace_root.join(format!("scenarios/{scenario_name}.json.gz"));

        let resolved_rpc_url = rpc_url
            .map(String::from)
            .or_else(|| std::env::var("RPC_URL").ok());

        #[cfg(feature = "rpc-fetch")]
        {
            self.accounts_db.scenario = if let Some(rpc_url) = resolved_rpc_url {
                Scenario::from_file_with_rpc(
                    scenario_path,
                    rpc_url,
                    self.config.allow_uninitialized_accounts_fetched,
                )
            } else {
                Scenario::from_file(scenario_path, self.config.allow_uninitialized_accounts_fetched)
            };
        }

        #[cfg(not(feature = "rpc-fetch"))]
        {
            assert!(
                resolved_rpc_url.is_none(),
                "an rpc_url was provided (or `RPC_URL` env var is set) but the `rpc-fetch` \
                 feature is disabled. Enable the `rpc-fetch` feature to fetch missing accounts \
                 from RPC."
            );
            self.accounts_db.scenario = Scenario::from_file(
                scenario_path,
                self.config.allow_uninitialized_accounts_fetched,
            );
        }
    }

    /// Loads a temporary scenario that fetches accounts from RPC without persisting to disk.
    ///
    /// If `rpc_url` is provided, uses that endpoint.
    /// If `rpc_url` is `None`, falls back to the `RPC_URL` environment variable.
    #[cfg(feature = "rpc-fetch")]
    pub fn load_temporary_scenario(&mut self, rpc_url: Option<&str>) {
        let resolved_rpc_url = rpc_url
            .map(String::from)
            .or_else(|| std::env::var("RPC_URL").ok())
            .expect("rpc_url must be provided or RPC_URL environment variable must be set");
        self.accounts_db.scenario =
            Scenario::rpc_only(resolved_rpc_url, self.config.allow_uninitialized_accounts_fetched);
    }

    pub fn process_instruction(&self, ixn: Instruction) -> InstructionProcessingResult {
        let transaction_accounts = self
            .accounts_db
            .accounts_for_instruction(self.config.allow_uninitialized_accounts_local, &ixn);

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

        let mut dedup_map =
            vec![u16::MAX; solana_transaction_context::MAX_ACCOUNTS_PER_TRANSACTION];
        for (idx, account) in instruction_accounts.iter().enumerate() {
            let index_in_instruction = dedup_map
                .get_mut(account.index_in_transaction as usize)
                .unwrap();
            if *index_in_instruction == u16::MAX {
                *index_in_instruction = idx as u16;
            }
        }

        transaction_context
            .configure_next_instruction(
                INSTRUCTION_PROGRAM_ID_INDEX as IndexOfAccount,
                instruction_accounts,
                dedup_map,
                std::borrow::Cow::Borrowed(&ixn.data),
            )
            .expect("Failed to configure instruction");

        let epoch_stake_callback = SeashellInvokeContextCallback { feature_set: &self.feature_set };
        let runtime_features = self.feature_set.runtime_features();
        let program_runtime_environments = ProgramRuntimeEnvironments::default();
        let mut programs = self.accounts_db.programs.clone();
        let mut invoke_context = InvokeContext::new(
            &mut transaction_context,
            &mut programs,
            EnvironmentConfig::new(
                Hash::default(),
                /* blockhash_lamports_per_signature */ 5000, // The default value
                &epoch_stake_callback,
                &runtime_features,
                &program_runtime_environments,
                &program_runtime_environments,
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
                                let account_ref = accounts
                                    .try_borrow(idx)
                                    .expect("Failed to borrow TransactionAccounts");
                                let account = AccountSharedData::create(
                                    account_ref.lamports(),
                                    account_ref.data().to_vec(),
                                    *account_ref.owner(),
                                    account_ref.executable(),
                                    account_ref.rent_epoch(),
                                );
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
            Err(e) => InstructionProcessingResult {
                compute_units_consumed,
                return_data,
                error: Some(InstructionProcessingError::InstructionError(e)),
                post_execution_accounts: Vec::default(),
            },
        }
    }

    /// Process multiple instructions atomically, as a single transaction.
    ///
    /// All instructions share one `TransactionContext`. If any instruction fails,
    /// none of the changes are committed (even with `memoize` enabled).
    pub fn process_instructions(&self, ixns: &[Instruction]) -> TransactionProcessingResult {
        if ixns.is_empty() {
            return TransactionProcessingResult {
                total_compute_units_consumed: 0,
                per_instruction_compute_units: vec![],
                return_data: vec![],
                error: None,
                post_execution_accounts: vec![],
            };
        }

        let transaction_account_keys = compile_transaction_account_keys(ixns);

        let sysvar_instructions_id = solana_sdk_ids::sysvar::instructions::id();
        let sysvar_instructions_account =
            SysvarInstructions::construct_instructions_account_for_transaction(ixns);

        let transaction_accounts: Vec<_> = transaction_account_keys
            .iter()
            .map(|pubkey| {
                if *pubkey == sysvar_instructions_id {
                    return (*pubkey, sysvar_instructions_account.clone());
                }
                (
                    *pubkey,
                    self.accounts_db
                        .resolve_account(pubkey, self.config.allow_uninitialized_accounts_local),
                )
            })
            .collect();

        let sysvar_cache = self
            .accounts_db
            .sysvars_for_instruction(&transaction_accounts);

        let mut transaction_context = TransactionContext::new(
            transaction_accounts,
            self.accounts_db.sysvars.rent(),
            self.compute_budget.max_instruction_stack_depth,
            self.compute_budget.max_instruction_trace_length,
        );

        let epoch_stake_callback = SeashellInvokeContextCallback { feature_set: &self.feature_set };
        let runtime_features = self.feature_set.runtime_features();
        let program_runtime_environments = ProgramRuntimeEnvironments::default();

        let mut programs = self.accounts_db.programs.clone();

        let mut total_compute_units = 0u64;
        let mut per_instruction_compute = Vec::with_capacity(ixns.len());

        for (idx, ixn) in ixns.iter().enumerate() {
            let (program_id_index, instruction_accounts) =
                compile_instruction_for_transaction(ixn, &transaction_account_keys);

            let mut dedup_map =
                vec![u16::MAX; solana_transaction_context::MAX_ACCOUNTS_PER_TRANSACTION];
            for (i, account) in instruction_accounts.iter().enumerate() {
                let slot = dedup_map
                    .get_mut(account.index_in_transaction as usize)
                    .unwrap();
                if *slot == u16::MAX {
                    *slot = i as u16;
                }
            }

            transaction_context
                .configure_next_instruction(
                    program_id_index,
                    instruction_accounts,
                    dedup_map,
                    std::borrow::Cow::Borrowed(&ixn.data),
                )
                .expect("Failed to configure instruction");

            let mut invoke_context = InvokeContext::new(
                &mut transaction_context,
                &mut programs,
                EnvironmentConfig::new(
                    Hash::default(),
                    /* blockhash_lamports_per_signature */ 5000,
                    &epoch_stake_callback,
                    &runtime_features,
                    &program_runtime_environments,
                    &program_runtime_environments,
                    &sysvar_cache,
                ),
                self.log_collector.clone(),
                self.compute_budget.to_budget(),
                self.compute_budget.to_cost(),
            );

            let mut compute_units_consumed = 0;

            let result = if invoke_context.is_precompile(&ixn.program_id) {
                let all_instruction_datas = ixns.iter().map(|ix| ix.data.as_slice());
                invoke_context.process_precompile(&ixn.program_id, &ixn.data, all_instruction_datas)
            } else {
                invoke_context.process_instruction(
                    &mut compute_units_consumed,
                    &mut ExecuteTimings::default(),
                )
            };

            total_compute_units += compute_units_consumed;
            per_instruction_compute.push(compute_units_consumed);

            // Drop invoke_context to release &mut borrow on transaction_context.
            drop(invoke_context);

            if let Err(e) = result {
                return TransactionProcessingResult {
                    total_compute_units_consumed: total_compute_units,
                    per_instruction_compute_units: per_instruction_compute,
                    return_data: transaction_context.get_return_data().1.to_owned(),
                    error: Some((idx, InstructionProcessingError::InstructionError(e))),
                    post_execution_accounts: Vec::default(),
                };
            }
        }

        let return_data = transaction_context.get_return_data().1.to_owned();
        let post_execution_accounts: Vec<(Pubkey, Account)> = transaction_account_keys
            .iter()
            .enumerate()
            .map(|(idx, pubkey)| {
                let accounts = transaction_context.accounts();
                let account_ref = accounts
                    .try_borrow(idx as IndexOfAccount)
                    .expect("Failed to borrow TransactionAccounts");
                let account = AccountSharedData::create(
                    account_ref.lamports(),
                    account_ref.data().to_vec(),
                    *account_ref.owner(),
                    account_ref.executable(),
                    account_ref.rent_epoch(),
                );
                if self.config.memoize {
                    self.set_account_from_account_shared_data(*pubkey, account.clone());
                }
                (*pubkey, account.into())
            })
            .collect();

        TransactionProcessingResult {
            total_compute_units_consumed: total_compute_units,
            per_instruction_compute_units: per_instruction_compute,
            return_data,
            error: None,
            post_execution_accounts,
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
        self.accounts_db.account_must(pubkey).into()
    }

    pub fn set_account(&self, pubkey: Pubkey, account: Account) {
        self.accounts_db.set_account(pubkey, account.into());
    }

    pub fn set_account_from_account_shared_data(&self, pubkey: Pubkey, account: AccountSharedData) {
        self.accounts_db.set_account(pubkey, account);
    }

    pub fn clear_non_program_accounts(&self) {
        self.accounts_db.clear_non_program_accounts();
    }

    pub fn warp(&self, slot: u64, timestamp: u64) {
        self.accounts_db.warp(slot, timestamp as i64);
    }
}

pub struct InstructionProcessingResult {
    pub compute_units_consumed: u64,
    pub return_data: Vec<u8>,
    pub error: Option<InstructionProcessingError>,
    pub post_execution_accounts: Vec<(Pubkey, Account)>,
}

pub struct TransactionProcessingResult {
    /// Total compute units consumed across all instructions.
    pub total_compute_units_consumed: u64,
    /// Compute units consumed by each instruction individually.
    pub per_instruction_compute_units: Vec<u64>,
    /// Return data from the last successfully executed instruction.
    pub return_data: Vec<u8>,
    /// If an instruction failed: `(instruction_index, error)`. `None` if all succeeded.
    pub error: Option<(usize, InstructionProcessingError)>,
    /// Post-execution account states. Empty if any instruction failed (atomic rollback).
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
            allow_uninitialized_accounts_local: false,
            allow_uninitialized_accounts_fetched: false,
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
        let seashell = Seashell::new();

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

        let reader = seashell.accounts_db.accounts.read();
        assert!(reader.contains_key(&tokenkeg));
        assert!(reader.contains_key(&token22));
        assert!(reader.contains_key(&associated_token));
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
            allow_uninitialized_accounts_local: false,
            allow_uninitialized_accounts_fetched: false,
        });

        let pubkey1 = Pubkey::from_str_const("B91piBSfCBRs5rUxCMRdJEGv7tNEnFxweWcdQJHJoFpi");
        let pubkey2 = Pubkey::from_str_const("6gAnjderE13TGGFeqdPVQ438jp2FPVeyXAszxKu9y338");

        // Load scenario with explicit RPC URL (no env var needed)
        seashell.load_scenario("test_scenario", Some("https://api.mainnet-beta.solana.com"));

        // Verify accounts are currently accessible
        // Will panic if not set
        seashell.account(&pubkey1);
        seashell.account(&pubkey2);

        // Drop seashell to trigger scenario save
        drop(seashell);

        // Create new seashell and load the saved scenario (no RPC needed, data is cached)
        let mut seashell2 = Seashell::new();
        seashell2.load_scenario("test_scenario", None);

        // Verify accounts were persisted and loaded
        // Will panic if not set
        seashell2.account(&pubkey1);
        seashell2.account(&pubkey2);
    }

    #[test]
    fn test_account_lookup_order() {
        let mut seashell = Seashell::new();

        let pubkey = Pubkey::new_unique();

        seashell.airdrop(pubkey, 1000);
        assert_eq!(seashell.account(&pubkey).lamports(), 1000);

        seashell.load_scenario("test_override", None);

        let override_account =
            AccountSharedData::new(2000, 0, &solana_sdk_ids::system_program::id());
        seashell
            .accounts_db
            .scenario
            .insert(pubkey, override_account);

        assert_eq!(seashell.account(&pubkey).lamports(), 2000);
    }

    #[test]
    #[should_panic(expected = "Account not found")]
    fn test_missing_account_without_rpc() {
        let mut seashell = Seashell::new();

        // Load scenario without RPC — no env var needed
        seashell.load_scenario("test_no_rpc", None);

        let missing_pubkey = Pubkey::from_str_const("NoShot1111111111111111111111111111111111111");
        seashell.account(&missing_pubkey);
    }

    #[test]
    fn test_spl_transfer_p_token() {
        crate::set_log();
        let mut seashell = Seashell::new();
        seashell.use_p_token();
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
        assert_eq!(result.compute_units_consumed, 82);

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
    fn test_process_instructions_native_transfer() {
        crate::set_log();
        let mut seashell = Seashell::new();

        let from = solana_pubkey::Pubkey::new_unique();
        let to_a = solana_pubkey::Pubkey::new_unique();
        let to_b = solana_pubkey::Pubkey::new_unique();
        seashell.airdrop(from, 1000);
        seashell.accounts_db.set_account_mock(to_a);
        seashell.accounts_db.set_account_mock(to_b);
        println!("Airdropped 1000 lamports to {from}");

        let mut data_a = Vec::with_capacity(12);
        data_a.extend_from_slice(&2u32.to_le_bytes());
        data_a.extend_from_slice(&600u64.to_le_bytes());

        let mut data_b = Vec::with_capacity(12);
        data_b.extend_from_slice(&2u32.to_le_bytes());
        data_b.extend_from_slice(&400u64.to_le_bytes());

        let ix_a = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(from, true), AccountMeta::new(to_a, false)],
            data: data_a,
        };
        let ix_b = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(from, true), AccountMeta::new(to_b, false)],
            data: data_b,
        };

        let result = seashell.process_instructions(&[ix_a, ix_b]);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.per_instruction_compute_units.len(), 2);

        let post_from = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == from)
            .expect("Resulting account should exist")
            .to_owned()
            .1;
        assert_eq!(
            post_from.lamports(),
            0,
            "Expected from account to have 0 lamports after both transfers"
        );

        let post_to_a = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == to_a)
            .expect("Resulting account should exist")
            .to_owned()
            .1;
        assert_eq!(
            post_to_a.lamports(),
            600,
            "Expected to_a account to have 600 lamports after transfer"
        );

        let post_to_b = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == to_b)
            .expect("Resulting account should exist")
            .to_owned()
            .1;
        assert_eq!(
            post_to_b.lamports(),
            400,
            "Expected to_b account to have 400 lamports after transfer"
        );

        assert!(
            result.return_data.is_empty(),
            "Expected no return data, got: {:?}",
            result.return_data
        );
    }

    #[test]
    fn test_process_instructions_atomic_rollback() {
        crate::set_log();
        let mut seashell = Seashell::new_with_config(Config {
            memoize: true,
            allow_uninitialized_accounts_local: false,
            allow_uninitialized_accounts_fetched: false,
        });

        let from = solana_pubkey::Pubkey::new_unique();
        let to = solana_pubkey::Pubkey::new_unique();
        seashell.airdrop(from, 1000);
        seashell.accounts_db.set_account_mock(to);

        let mut data_a = Vec::with_capacity(12);
        data_a.extend_from_slice(&2u32.to_le_bytes());
        data_a.extend_from_slice(&600u64.to_le_bytes());

        let mut data_b = Vec::with_capacity(12);
        data_b.extend_from_slice(&2u32.to_le_bytes());
        data_b.extend_from_slice(&600u64.to_le_bytes());

        let ix_a = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(from, true), AccountMeta::new(to, false)],
            data: data_a,
        };
        let ix_b = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(from, true), AccountMeta::new(to, false)],
            data: data_b,
        };

        let result = seashell.process_instructions(&[ix_a, ix_b]);
        assert!(result.error.is_some(), "Expected second instruction to fail");
        assert_eq!(result.error.as_ref().unwrap().0, 1, "Expected failure index to be 1");

        let post_from = seashell.account(&from);
        assert_eq!(
            post_from.lamports(),
            1000,
            "Expected from account to still have 1000 lamports after atomic rollback"
        );
    }

    #[test]
    fn test_process_instructions_memoize() {
        crate::set_log();
        let mut seashell = Seashell::new_with_config(Config {
            memoize: true,
            allow_uninitialized_accounts_local: false,
            allow_uninitialized_accounts_fetched: false,
        });

        let from = solana_pubkey::Pubkey::new_unique();
        let to = solana_pubkey::Pubkey::new_unique();
        seashell.airdrop(from, 1000);
        seashell.accounts_db.set_account_mock(to);

        let mut data = Vec::with_capacity(12);
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&500u64.to_le_bytes());

        let ixn = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(from, true), AccountMeta::new(to, false)],
            data,
        };

        let result = seashell.process_instructions(&[ixn]);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);

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
    fn test_process_instructions_empty() {
        let seashell = Seashell::new();
        let result = seashell.process_instructions(&[]);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.total_compute_units_consumed, 0);
        assert!(result.post_execution_accounts.is_empty());
    }

    #[test]
    fn test_process_instructions_allow_uninitialized_accounts() {
        crate::set_log();
        let mut seashell = Seashell::new_with_config(Config {
            memoize: false,
            allow_uninitialized_accounts_local: true,
            allow_uninitialized_accounts_fetched: false,
        });

        let from = solana_pubkey::Pubkey::new_unique();
        let to = solana_pubkey::Pubkey::new_unique();
        seashell.airdrop(from, 1000);

        let mut data = Vec::with_capacity(12);
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&500u64.to_le_bytes());

        let ixn = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(from, true), AccountMeta::new(to, false)],
            data,
        };

        let result = seashell.process_instructions(&[ixn]);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);

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
    }

    #[test]
    fn test_process_instructions_precompile() {
        const MESSAGE_LENGTH: usize = 128;
        crate::set_log();
        let mut seashell = Seashell::new();

        use rand::{thread_rng, Rng};
        let mut rng = thread_rng();

        let from = solana_pubkey::Pubkey::new_unique();
        let to = solana_pubkey::Pubkey::new_unique();
        seashell.airdrop(from, 1000);
        seashell.accounts_db.set_account_mock(to);

        let mut transfer_data = Vec::with_capacity(12);
        transfer_data.extend_from_slice(&2u32.to_le_bytes());
        transfer_data.extend_from_slice(&500u64.to_le_bytes());

        let transfer_ixn = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(from, true), AccountMeta::new(to, false)],
            data: transfer_data,
        };

        use ed25519_dalek::Signer;
        let privkey = ed25519_dalek::Keypair::generate(&mut rng);
        let message: Vec<u8> = (0..MESSAGE_LENGTH).map(|_| rng.gen_range(0, 255)).collect();
        let signature = privkey.sign(&message).to_bytes();
        let pubkey = privkey.public.to_bytes();
        let ed25519_ixn = solana_ed25519_program::new_ed25519_instruction_with_signature(
            &message, &signature, &pubkey,
        );

        let result = seashell.process_instructions(&[transfer_ixn, ed25519_ixn]);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.per_instruction_compute_units.len(), 2);
        assert_eq!(
            result.per_instruction_compute_units[1], 0,
            "Expected ed25519 precompile to consume 0 compute units"
        );

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
    }

    #[test]
    fn test_process_instructions_cpi_via_ata_program() {
        crate::set_log();
        let mut seashell = Seashell::new();

        let payer = solana_pubkey::Pubkey::new_unique();
        let mint = solana_pubkey::Pubkey::new_unique();
        let mint_authority = solana_pubkey::Pubkey::new_unique();
        let wallet = solana_pubkey::Pubkey::new_unique();

        const MINT_LEN: u64 = 82;
        const MINT_RENT: u64 = 1_461_600;
        const TOKEN_ACCOUNT_LEN: usize = 165;

        seashell.airdrop(payer, 10_000_000);
        seashell.accounts_db.set_account_mock(wallet);
        seashell
            .accounts_db
            .set_account(mint, AccountSharedData::new(0, 0, &solana_sdk_ids::system_program::id()));

        let (ata, _) = Pubkey::find_program_address(
            &[wallet.as_ref(), crate::spl::TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
            &crate::spl::ASSOCIATED_TOKEN_PROGRAM_ID,
        );
        seashell
            .accounts_db
            .set_account(ata, AccountSharedData::new(0, 0, &solana_sdk_ids::system_program::id()));

        let mut create_mint_data = Vec::with_capacity(52);
        create_mint_data.extend_from_slice(&0u32.to_le_bytes());
        create_mint_data.extend_from_slice(&MINT_RENT.to_le_bytes());
        create_mint_data.extend_from_slice(&MINT_LEN.to_le_bytes());
        create_mint_data.extend_from_slice(&crate::spl::TOKEN_PROGRAM_ID.to_bytes());

        let create_mint_ixn = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(payer, true), AccountMeta::new(mint, true)],
            data: create_mint_data,
        };

        let mut init_mint_data = Vec::with_capacity(35);
        init_mint_data.push(20);
        init_mint_data.push(6);
        init_mint_data.extend_from_slice(&mint_authority.to_bytes());
        init_mint_data.push(0);

        let init_mint_ixn = Instruction {
            program_id: crate::spl::TOKEN_PROGRAM_ID,
            accounts: vec![AccountMeta::new(mint, false)],
            data: init_mint_data,
        };

        let create_ata_ixn = Instruction {
            program_id: crate::spl::ASSOCIATED_TOKEN_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(ata, false),
                AccountMeta::new_readonly(wallet, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(solana_sdk_ids::system_program::id(), false),
                AccountMeta::new_readonly(crate::spl::TOKEN_PROGRAM_ID, false),
            ],
            data: vec![],
        };

        let result =
            seashell.process_instructions(&[create_mint_ixn, init_mint_ixn, create_ata_ixn]);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.per_instruction_compute_units.len(), 3);

        let post_ata = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == ata)
            .expect("Resulting account should exist")
            .to_owned()
            .1;
        assert_eq!(
            post_ata.owner,
            crate::spl::TOKEN_PROGRAM_ID,
            "Expected ATA account to be owned by the token program"
        );
        assert_eq!(
            post_ata.data.len(),
            TOKEN_ACCOUNT_LEN,
            "Expected ATA account to have token-account-sized data"
        );
        assert_eq!(
            &post_ata.data[0..32],
            &mint.to_bytes(),
            "Expected ATA mint field to match mint pubkey"
        );
        assert_eq!(
            &post_ata.data[32..64],
            &wallet.to_bytes(),
            "Expected ATA owner field to match wallet pubkey"
        );
    }

    #[test]
    fn test_process_instructions_create_and_init_mint() {
        crate::set_log();
        let mut seashell = Seashell::new();

        let payer = solana_pubkey::Pubkey::new_unique();
        let mint = solana_pubkey::Pubkey::new_unique();
        let mint_authority = solana_pubkey::Pubkey::new_unique();

        const MINT_LEN: u64 = 82;
        const MINT_RENT: u64 = 1_461_600;

        seashell.airdrop(payer, 10_000_000);
        seashell
            .accounts_db
            .set_account(mint, AccountSharedData::new(0, 0, &solana_sdk_ids::system_program::id()));

        let mut create_data = Vec::with_capacity(52);
        create_data.extend_from_slice(&0u32.to_le_bytes());
        create_data.extend_from_slice(&MINT_RENT.to_le_bytes());
        create_data.extend_from_slice(&MINT_LEN.to_le_bytes());
        create_data.extend_from_slice(&crate::spl::TOKEN_PROGRAM_ID.to_bytes());

        let create_ixn = Instruction {
            program_id: solana_sdk_ids::system_program::id(),
            accounts: vec![AccountMeta::new(payer, true), AccountMeta::new(mint, true)],
            data: create_data,
        };

        let mut init_data = Vec::with_capacity(35);
        init_data.push(20);
        init_data.push(6);
        init_data.extend_from_slice(&mint_authority.to_bytes());
        init_data.push(0);

        let init_ixn = Instruction {
            program_id: crate::spl::TOKEN_PROGRAM_ID,
            accounts: vec![AccountMeta::new(mint, false)],
            data: init_data,
        };

        let result = seashell.process_instructions(&[create_ixn, init_ixn]);
        assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
        assert_eq!(result.per_instruction_compute_units.len(), 2);

        let post_mint = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == mint)
            .expect("Resulting account should exist")
            .to_owned()
            .1;
        assert_eq!(
            post_mint.owner,
            crate::spl::TOKEN_PROGRAM_ID,
            "Expected mint account to be owned by the token program"
        );
        assert_eq!(post_mint.lamports, MINT_RENT, "Expected mint account to be funded with rent");
        assert_eq!(
            post_mint.data.len(),
            MINT_LEN as usize,
            "Expected mint account to have mint-sized data"
        );
        assert_eq!(
            &post_mint.data[0..4],
            &1u32.to_le_bytes(),
            "Expected mint_authority COption tag to be Some"
        );
        assert_eq!(
            &post_mint.data[4..36],
            &mint_authority.to_bytes(),
            "Expected mint_authority pubkey to match"
        );
        assert_eq!(&post_mint.data[36..44], &0u64.to_le_bytes(), "Expected mint supply to be 0");
        assert_eq!(post_mint.data[44], 6, "Expected mint decimals to be 6");
        assert_eq!(post_mint.data[45], 1, "Expected mint is_initialized to be true");
        assert_eq!(
            &post_mint.data[46..50],
            &0u32.to_le_bytes(),
            "Expected freeze_authority COption tag to be None"
        );
    }
}
