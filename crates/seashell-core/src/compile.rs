use indexmap::IndexMap;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_transaction_context::{IndexOfAccount, InstructionAccount};

pub const INSTRUCTION_PROGRAM_ID_INDEX: u16 = 0;

fn merge_account_privileges(ixn: &Instruction) -> IndexMap<Pubkey, (bool, bool)> {
    let mut privileges: IndexMap<Pubkey, (bool, bool)> = IndexMap::new();
    for meta in &ixn.accounts {
        privileges
            .entry(meta.pubkey)
            .and_modify(|e| {
                e.0 |= meta.is_signer;
                e.1 |= meta.is_writable;
            })
            .or_insert((meta.is_signer, meta.is_writable));
    }
    privileges
}

/// Deduplicated account keys across all instructions, preserving order of first appearance.
pub fn compile_transaction_account_keys(ixns: &[Instruction]) -> Vec<Pubkey> {
    let mut seen: IndexMap<Pubkey, ()> = IndexMap::new();
    for ixn in ixns {
        seen.entry(ixn.program_id).or_insert(());
        for meta in &ixn.accounts {
            seen.entry(meta.pubkey).or_insert(());
        }
    }
    seen.keys().copied().collect()
}

/// Like [`compile_accounts_for_instruction`], but indices reference a shared transaction account list.
pub fn compile_instruction_for_transaction(
    ixn: &Instruction,
    transaction_account_keys: &[Pubkey],
) -> (IndexOfAccount, Vec<InstructionAccount>) {
    let program_id_index = transaction_account_keys
        .iter()
        .position(|k| *k == ixn.program_id)
        .expect("program_id must be present in transaction_account_keys")
        as IndexOfAccount;

    let account_privileges = merge_account_privileges(ixn);

    let instruction_accounts = ixn
        .accounts
        .iter()
        .map(|meta| {
            let tx_index = transaction_account_keys
                .iter()
                .position(|k| *k == meta.pubkey)
                .expect("account must be present in transaction_account_keys")
                as IndexOfAccount;
            let (is_signer, is_writable) = account_privileges.get(&meta.pubkey).unwrap();
            InstructionAccount::new(tx_index, *is_signer, *is_writable)
        })
        .collect();

    (program_id_index, instruction_accounts)
}

pub fn compile_accounts_for_instruction(ixn: &Instruction) -> Vec<InstructionAccount> {
    let mut account_map = merge_account_privileges(ixn);
    account_map.entry(ixn.program_id).or_insert((false, false));

    let mut transaction_accounts = vec![ixn.program_id];
    for account_meta in &ixn.accounts {
        transaction_accounts.push(account_meta.pubkey);
    }

    let mut pubkey_to_first_tx_index: IndexMap<Pubkey, u8> = IndexMap::new();
    for (idx, pubkey) in transaction_accounts.iter().enumerate() {
        pubkey_to_first_tx_index.entry(*pubkey).or_insert(idx as u8);
    }

    ixn.accounts
        .iter()
        .map(|account_meta| {
            let first_tx_idx = *pubkey_to_first_tx_index.get(&account_meta.pubkey).unwrap();

            let (is_signer, is_writable) = account_map.get(&account_meta.pubkey).unwrap();

            InstructionAccount::new(first_tx_idx as IndexOfAccount, *is_signer, *is_writable)
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
        assert!(acc.is_signer());
        assert!(acc.is_writable());
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
        assert!(acc0.is_signer()); // highest privilege wins
        assert!(acc0.is_writable()); // highest privilege wins

        // Account B first occurrence (transaction index 2)
        let acc1 = &result[1];
        assert_eq!(acc1.index_in_transaction, 2);
        assert!(acc1.is_signer()); // highest privilege from later usage
        assert!(acc1.is_writable()); // highest privilege from later usage

        // Account A second occurrence (same transaction index)
        let acc2 = &result[2];
        assert_eq!(acc2.index_in_transaction, 1); // same as first A
        assert!(acc2.is_signer()); // same privileges as first A
        assert!(acc2.is_writable());

        // Account B second occurrence (same transaction index)
        let acc3 = &result[3];
        assert_eq!(acc3.index_in_transaction, 2); // same as first B
        assert!(acc3.is_signer());
        assert!(acc3.is_writable());
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
            assert!(acc.is_signer()); // escalated from false to true
            assert!(acc.is_writable()); // escalated from false to true
        }
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
            1, // user_account first occurrence
            2, // system_program first occurrence
            3, // token_account first occurrence
            1, // user_account again -> points to first occurrence
            2, // system_program again -> points to first occurrence
            3, // token_account again -> points to first occurrence
        ];

        for (i, exp_tx_idx) in expected.iter().enumerate() {
            let acc = &result[i];
            assert_eq!(
                acc.index_in_transaction as usize, *exp_tx_idx,
                "Wrong transaction index at position {i}",
            );
        }

        // Check privilege escalation
        // user_account: should be signer (from first usage) and writable (from first usage)
        assert!(result[0].is_signer());
        assert!(result[0].is_writable());
        assert!(result[3].is_signer()); // same account, same privileges
        assert!(result[3].is_writable());

        // system_program: should be readonly, non-signer
        assert!(!result[1].is_signer());
        assert!(!result[1].is_writable());
        assert!(!result[4].is_signer());
        assert!(!result[4].is_writable());

        // token_account: should be signer (from second usage) and writable (from both usages)
        assert!(result[2].is_signer());
        assert!(result[2].is_writable());
        assert!(result[5].is_signer());
        assert!(result[5].is_writable());
    }

    #[test]
    fn test_compile_transaction_account_keys() {
        let program_a = Pubkey::new_unique();
        let program_b = Pubkey::new_unique();
        let account1 = Pubkey::new_unique();
        let account2 = Pubkey::new_unique();
        let shared = Pubkey::new_unique();

        let ix1 = Instruction {
            program_id: program_a,
            accounts: vec![AccountMeta::new(account1, true), AccountMeta::new(shared, false)],
            data: vec![],
        };
        let ix2 = Instruction {
            program_id: program_b,
            accounts: vec![AccountMeta::new(account2, true), AccountMeta::new(shared, false)],
            data: vec![],
        };

        let keys = compile_transaction_account_keys(&[ix1, ix2]);
        // Deduplicated, order of first appearance
        assert_eq!(keys.len(), 5); // program_a, account1, shared, program_b, account2
        assert_eq!(keys[0], program_a);
        assert_eq!(keys[1], account1);
        assert_eq!(keys[2], shared);
        assert_eq!(keys[3], program_b);
        assert_eq!(keys[4], account2);
    }

    #[test]
    fn test_compile_instruction_for_transaction() {
        let program_a = Pubkey::new_unique();
        let program_b = Pubkey::new_unique();
        let account1 = Pubkey::new_unique();
        let shared = Pubkey::new_unique();

        let ix1 = Instruction {
            program_id: program_a,
            accounts: vec![AccountMeta::new(account1, true), AccountMeta::new(shared, false)],
            data: vec![],
        };
        let ix2 = Instruction {
            program_id: program_b,
            accounts: vec![AccountMeta::new(shared, true)],
            data: vec![],
        };

        let keys = compile_transaction_account_keys(&[ix1.clone(), ix2.clone()]);
        // keys: [program_a(0), account1(1), shared(2), program_b(3)]

        let (pid_idx1, accs1) = compile_instruction_for_transaction(&ix1, &keys);
        assert_eq!(pid_idx1, 0); // program_a is at index 0
        assert_eq!(accs1.len(), 2);
        assert_eq!(accs1[0].index_in_transaction, 1); // account1
        assert!(accs1[0].is_signer());
        assert!(accs1[0].is_writable());
        assert_eq!(accs1[1].index_in_transaction, 2); // shared
        assert!(!accs1[1].is_signer());
        assert!(accs1[1].is_writable());

        let (pid_idx2, accs2) = compile_instruction_for_transaction(&ix2, &keys);
        assert_eq!(pid_idx2, 3); // program_b is at index 3
        assert_eq!(accs2.len(), 1);
        assert_eq!(accs2[0].index_in_transaction, 2); // shared
        assert!(accs2[0].is_signer());
        assert!(accs2[0].is_writable());
    }
}
