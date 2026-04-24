use seashell::{try_find_workspace_root, Seashell};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

#[test]
fn test_process_instructions_sysvar_instructions_current_index() {
    let mut seashell = Seashell::new();
    seashell.enable_log_collector();

    let sysvar_ixns_so = try_find_workspace_root()
        .unwrap()
        .join("programs/sysvar_ixns/target/deploy/sysvar_ixns.so");
    let program_bytes = std::fs::read(&sysvar_ixns_so)
        .expect("sysvar_ixns.so not built; run `cargo build-sbf` in programs/sysvar_ixns");
    let program_id = Pubkey::new_unique();
    seashell.load_program_from_bytes(program_id, &program_bytes);

    let from = Pubkey::new_unique();
    let to = Pubkey::new_unique();
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

    let sysvar_ixns_ixn = Instruction {
        program_id,
        accounts: vec![AccountMeta::new_readonly(
            solana_sdk_ids::sysvar::instructions::id(),
            false,
        )],
        data: Vec::new(),
    };

    let result = seashell.process_instructions(&[transfer_ixn, sysvar_ixns_ixn]);
    assert!(result.error.is_none(), "Expected no error, got: {:?}", result.error);
    assert_eq!(result.per_instruction_compute_units.len(), 2);

    let logs = seashell
        .logs()
        .expect("Expected log collector to be enabled");
    let program_id_base58 = program_id.to_string();
    assert!(
        logs.iter().any(|line| line.contains(&program_id_base58)),
        "Expected logs to contain sysvar_ixns program_id (proving sysvar::instructions \
         current index was updated to 1). Logs: {logs:#?}"
    );
}
