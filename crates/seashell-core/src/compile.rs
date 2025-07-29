use indexmap::IndexMap;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_transaction_context::{IndexOfAccount, InstructionAccount};

pub const INSTRUCTION_PROGRAM_ID_INDEX: u16 = 0;

pub fn compile_accounts_for_instruction(ixn: &Instruction) -> Vec<InstructionAccount> {
    // IndexMap preserves insertion order so program_id index will always be 0
    let mut account_map: IndexMap<Pubkey, (bool, bool)> = IndexMap::new();

    account_map.insert(ixn.program_id, (false, false));

    for account in &ixn.accounts {
        account_map
            .entry(account.pubkey)
            .and_modify(|e| {
                e.0 |= account.is_signer;
                e.1 |= account.is_writable;
            })
            .or_insert((account.is_signer, account.is_writable));
    }

    let is_signer = |map: &IndexMap<Pubkey, (bool, bool)>, idx: usize| -> bool {
        *map.get_index(idx)
            .map(|(_, (is_signer, _))| is_signer)
            .unwrap()
    };

    let is_writable = |map: &IndexMap<Pubkey, (bool, bool)>, idx: usize| -> bool {
        *map.get_index(idx)
            .map(|(_, (_, is_writable))| is_writable)
            .unwrap()
    };

    let account_indices = ixn
        .accounts
        .iter()
        .map(|account_meta| account_map.get_index_of(&account_meta.pubkey).unwrap() as u8)
        .collect::<Vec<_>>();

    account_indices
        .iter()
        .enumerate()
        .map(|(idx, global_idx)| {
            let index_in_callee = account_indices
                .iter()
                .take(idx)
                .position(|&acc_idx| acc_idx == *global_idx)
                .unwrap_or(idx) as IndexOfAccount;

            InstructionAccount {
                index_in_transaction: *global_idx as IndexOfAccount,
                index_in_caller: *global_idx as IndexOfAccount,
                index_in_callee,
                is_signer: is_signer(&account_map, *global_idx as usize),
                is_writable: is_writable(&account_map, *global_idx as usize),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use solana_instruction::{AccountMeta, Instruction};
    use solana_pubkey::Pubkey;

    use super::*;

    #[test]
    fn test_single_account_instruction() {
        let program_id = Pubkey::new_unique();
        let account = Pubkey::new_unique();

        let instruction = Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(account, true), // writable signer
            ],
            data: vec![],
        };

        let result = compile_accounts_for_instruction(&instruction);

        assert_eq!(result.len(), 1);

        let acc = &result[0];
        assert_eq!(acc.index_in_transaction, 1); // program_id is 0, this account is 1
        assert_eq!(acc.index_in_caller, 1);
        assert_eq!(acc.index_in_callee, 0); // first account in instruction
        assert!(acc.is_signer);
        assert!(acc.is_writable);
    }

    #[test]
    fn test_duplicate_accounts_in_instruction() {
        let program_id = Pubkey::new_unique();
        let account_a = Pubkey::new_unique();
        let account_b = Pubkey::new_unique();

        let instruction = Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(account_a, true), // pos 0: A (signer, writable)
                AccountMeta::new_readonly(account_b, false), // pos 1: B (readonly)
                AccountMeta::new_readonly(account_a, false), // pos 2: A again (readonly, non-signer)
                AccountMeta::new(account_b, true),           // pos 3: B again (signer, writable)
            ],
            data: vec![],
        };

        let result = compile_accounts_for_instruction(&instruction);

        assert_eq!(result.len(), 4);

        // Account A first occurrence (transaction index 1)
        let acc0 = &result[0];
        assert_eq!(acc0.index_in_transaction, 1);
        assert_eq!(acc0.index_in_caller, 1);
        assert_eq!(acc0.index_in_callee, 0); // first occurrence
        assert!(acc0.is_signer); // highest privilege wins
        assert!(acc0.is_writable); // highest privilege wins

        // Account B first occurrence (transaction index 2)
        let acc1 = &result[1];
        assert_eq!(acc1.index_in_transaction, 2);
        assert_eq!(acc1.index_in_caller, 2);
        assert_eq!(acc1.index_in_callee, 1); // first occurrence
        assert!(acc1.is_signer); // highest privilege from later usage
        assert!(acc1.is_writable); // highest privilege from later usage

        // Account A second occurrence (same transaction index)
        let acc2 = &result[2];
        assert_eq!(acc2.index_in_transaction, 1); // same as first A
        assert_eq!(acc2.index_in_caller, 1);
        assert_eq!(acc2.index_in_callee, 0); // points to first occurrence
        assert!(acc2.is_signer); // same privileges as first A
        assert!(acc2.is_writable);

        // Account B second occurrence (same transaction index)
        let acc3 = &result[3];
        assert_eq!(acc3.index_in_transaction, 2); // same as first B
        assert_eq!(acc3.index_in_caller, 2);
        assert_eq!(acc3.index_in_callee, 1); // points to first occurrence
        assert!(acc3.is_signer);
        assert!(acc3.is_writable);
    }

    #[test]
    fn test_privilege_escalation() {
        let program_id = Pubkey::new_unique();
        let account = Pubkey::new_unique();

        let instruction = Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new_readonly(account, false), // readonly, non-signer
                AccountMeta::new(account, true),           // writable, signer
            ],
            data: vec![],
        };

        let result = compile_accounts_for_instruction(&instruction);

        assert_eq!(result.len(), 2);

        // Both should have escalated privileges
        for acc in &result {
            assert_eq!(acc.index_in_transaction, 1); // same account
            assert!(acc.is_signer); // escalated from false to true
            assert!(acc.is_writable); // escalated from false to true
        }

        // Check callee indices
        assert_eq!(result[0].index_in_callee, 0); // first occurrence
        assert_eq!(result[1].index_in_callee, 0); // points to first occurrence
    }

    #[test]
    fn test_empty_instruction() {
        let program_id = Pubkey::new_unique();

        let instruction = Instruction {
            program_id,
            accounts: vec![], // no accounts
            data: vec![],
        };

        let result = compile_accounts_for_instruction(&instruction);

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_complex_scenario() {
        let program_id = Pubkey::new_unique();
        let system_program = Pubkey::new_unique();
        let user_account = Pubkey::new_unique();
        let token_account = Pubkey::new_unique();

        let instruction = Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(user_account, true), // 0: user (signer, writable)
                AccountMeta::new_readonly(system_program, false), // 1: system program (readonly)
                AccountMeta::new(token_account, false), // 2: token account (writable)
                AccountMeta::new_readonly(user_account, false), // 3: user again (readonly, non-signer)
                AccountMeta::new_readonly(system_program, false), // 4: system program again
                AccountMeta::new(token_account, true), // 5: token account (writable, signer)
            ],
            data: vec![],
        };

        let result = compile_accounts_for_instruction(&instruction);

        assert_eq!(result.len(), 6);

        // Expected transaction indices: program_id=0, user_account=1, system_program=2, token_account=3
        // Expected callee indices based on first occurrence

        let expected = [
            (1, 0), // user_account first occurrence
            (2, 1), // system_program first occurrence
            (3, 2), // token_account first occurrence
            (1, 0), // user_account again -> points to first occurrence
            (2, 1), // system_program again -> points to first occurrence
            (3, 2), // token_account again -> points to first occurrence
        ];

        for (i, (exp_tx_idx, exp_callee_idx)) in expected.iter().enumerate() {
            let acc = &result[i];
            assert_eq!(
                acc.index_in_transaction as usize, *exp_tx_idx,
                "Wrong transaction index at position {i}",
            );
            assert_eq!(
                acc.index_in_callee as usize, *exp_callee_idx,
                "Wrong callee index at position {i}",
            );
        }

        // Check privilege escalation
        // user_account: should be signer (from first usage) and writable (from first usage)
        assert!(result[0].is_signer);
        assert!(result[0].is_writable);
        assert!(result[3].is_signer); // same account, same privileges
        assert!(result[3].is_writable);

        // system_program: should be readonly, non-signer
        assert!(!result[1].is_signer);
        assert!(!result[1].is_writable);
        assert!(!result[4].is_signer);
        assert!(!result[4].is_writable);

        // token_account: should be signer (from second usage) and writable (from both usages)
        assert!(result[2].is_signer);
        assert!(result[2].is_writable);
        assert!(result[5].is_signer);
        assert!(result[5].is_writable);
    }
}
