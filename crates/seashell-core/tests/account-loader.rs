use std::{cell::RefCell, rc::Rc};

use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;



#[test]
fn test_account_loader() {
    let mut seashell = Seashell::new();
    let account_loader_out_dir = try_find_workspace_root()
        .unwrap()
        .join("programs/account-loader/target/deploy");
    unsafe { std::env::set_var("SBF_OUT_DIR", account_loader_out_dir.to_str().unwrap()) }
    let program_id = Pubkey::new_unique();
    seashell.load_program_from_environment("account_loader", program_id).unwrap();

    let log_collector = Rc::new(RefCell::new(solana_log_collector::LogCollector::default()));
    seashell.log_collector = Some(Rc::clone(&log_collector));

    let mut pubkey_order = Vec::new();
    let account_metas: [AccountMeta; 50] = std::array::from_fn(|_| {
        let pubkey = Pubkey::new_unique();
        pubkey_order.push(pubkey);
        AccountMeta::new(pubkey, false)
    });

    for meta in &account_metas {
        seashell.set_account(meta.pubkey, Account {
            lamports: 1000,
            data: vec![],
            owner: Pubkey::new_unique(),
            executable: false,
            rent_epoch: 0
        });
    }

    let instruction = Instruction {
        program_id,
        accounts: account_metas.to_vec(),
        data: vec![]
    };

    seashell.process_instruction(instruction);

    let logs = log_collector.borrow().get_recorded_content().to_owned();

    let pubkeys: Vec<&str> = logs.iter()
        .skip(1)
        .filter_map(|line| line.split("Program log: ").last())
        .collect();

    for (pubkey_str, pubkey) in pubkeys.iter().zip(pubkey_order.iter()) {
        assert_eq!(pubkey_str, &pubkey.to_string())
    }
}


#[test]
fn test_account_loader_duplicate_accounts() {
    let mut seashell = Seashell::new();
    let account_loader_out_dir = try_find_workspace_root()
        .unwrap()
        .join("programs/account-loader/target/deploy");
    unsafe { std::env::set_var("SBF_OUT_DIR", account_loader_out_dir.to_str().unwrap()) }
    let program_id = Pubkey::new_unique();
    seashell.load_program_from_environment("account_loader", program_id).unwrap();

    let log_collector = Rc::new(RefCell::new(solana_log_collector::LogCollector::default()));
    seashell.log_collector = Some(Rc::clone(&log_collector));

    let mut pubkey_order = Vec::new();
    let pubkey = Pubkey::new_unique();
    let account_metas: [AccountMeta; 50] = std::array::from_fn(|_| {
        pubkey_order.push(pubkey);
        AccountMeta::new(pubkey, false)
    });

    for meta in &account_metas {
        seashell.set_account(meta.pubkey, Account {
            lamports: 1000,
            data: vec![],
            owner: Pubkey::new_unique(),
            executable: false,
            rent_epoch: 0
        });
    }

    let instruction = Instruction {
        program_id,
        accounts: account_metas.to_vec(),
        data: vec![]
    };

    seashell.process_instruction(instruction);

    let logs = log_collector.borrow().get_recorded_content().to_owned();

    let pubkeys: Vec<&str> = logs.iter()
        .skip(1)
        .filter_map(|line| line.split("Program log: ").last())
        .collect();

    for (pubkey_str, pubkey) in pubkeys.iter().zip(pubkey_order.iter()) {
        assert_eq!(pubkey_str, &pubkey.to_string())
    }
}