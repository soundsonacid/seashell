use seashell::{try_find_workspace_root, Seashell};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

#[test]
fn test_sysvar_ixns() {
    let mut seashell = Seashell::new();
    let account_loader_out_dir = try_find_workspace_root()
        .unwrap()
        .join("programs/sysvar_ixns/target/deploy");
    unsafe { std::env::set_var("SBF_OUT_DIR", account_loader_out_dir.to_str().unwrap()) }
    let program_id = Pubkey::new_unique();
    seashell
        .load_program_from_environment("sysvar_ixns", program_id)
        .unwrap();

    seashell.enable_log_collector();

    let accounts = vec![
        AccountMeta::new_readonly(solana_sdk_ids::sysvar::instructions::id(), false),
    ];

    let ixn = Instruction { program_id, accounts, data: Vec::new() };

    let result = seashell.process_instruction(ixn);

    assert!(result.error.is_none());
}