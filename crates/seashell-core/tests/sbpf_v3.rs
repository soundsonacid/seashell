use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sbpf::profiler::FrameKey;

#[test]
fn load_and_execute_sbpf_v3_elf() {
    let root = try_find_workspace_root().unwrap();
    let deploy_dir = root.join("programs/sbpf-v3/target/deploy");
    if !deploy_dir.join("sbpf_v3.so").is_file() {
        eprintln!("sbpf_v3.so not built; skipping");
        return;
    }

    let mut seashell = Seashell::new();
    seashell.enable_log_collector();
    unsafe { std::env::set_var("SBF_OUT_DIR", deploy_dir.to_str().unwrap()) }
    let program_id = Pubkey::new_unique();
    seashell
        .load_program_from_environment("sbpf_v3", program_id)
        .unwrap();

    let target = Pubkey::new_unique();
    seashell.set_account(
        target,
        Account {
            lamports: 1_000_000,
            data: vec![0u8; 8],
            owner: program_id,
            executable: false,
            rent_epoch: 0,
        },
    );

    let data = vec![1u8, 2, 3, 250];
    let expected: u64 = data.iter().map(|b| *b as u64).sum();
    let ixn = Instruction {
        program_id,
        accounts: vec![AccountMeta::new(target, false)],
        data,
    };
    let result = seashell.profile_instruction(ixn);
    assert!(result.error.is_none(), "{:?} logs: {:?}", result.error, seashell.logs());
    let post_target = result
        .post_execution_accounts
        .iter()
        .find(|(pubkey, _)| *pubkey == target)
        .expect("target account should exist")
        .1
        .clone();
    assert_eq!(post_target.data[..8], expected.to_le_bytes());

    let profiler = seashell.profiler.as_ref().expect("profiler should be populated");
    let total: u64 = profiler.nodes.iter().map(|n| n.self_cu).sum();
    assert_eq!(
        total, result.compute_units_consumed,
        "CCT self-CU must equal consumed CU"
    );
    assert!(profiler
        .nodes
        .iter()
        .any(|n| n.key == FrameKey::Program(program_id.to_bytes())));
}
