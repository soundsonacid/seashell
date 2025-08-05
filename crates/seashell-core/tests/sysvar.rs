use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_clock::Clock;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_sysvar_id::{SysvarId, ID as SYSVAR};

#[test]
fn test_sysvar_override() {
    let mut seashell = Seashell::new();
    let account_loader_out_dir = try_find_workspace_root()
        .unwrap()
        .join("programs/sysvar/target/deploy");
    unsafe { std::env::set_var("SBF_OUT_DIR", account_loader_out_dir.to_str().unwrap()) }
    let program_id = Pubkey::new_unique();
    seashell
        .load_program_from_environment("sysvar", program_id)
        .unwrap();

    seashell.enable_log_collector();

    let expected_clock = Clock {
        slot: 1,
        epoch_start_timestamp: 2,
        epoch: 3,
        leader_schedule_epoch: 4,
        unix_timestamp: 5,
    };

    let expected_clock_bytes = bincode::serialize(&expected_clock).unwrap();

    let expected_clock_account = Account {
        lamports: 1000,
        data: expected_clock_bytes,
        owner: SYSVAR,
        executable: false,
        rent_epoch: 0,
    };

    seashell.set_account(Clock::id(), expected_clock_account);

    let ixn = Instruction { program_id, accounts: Vec::new(), data: Vec::new() };

    let result = seashell.process_instruction(ixn);

    assert!(result.error.is_none());

    let logs = seashell.logs().expect("log collector was set");
    let (slot, epoch, epoch_start_ts, lse, ts) = parse_logs(&logs);

    assert_eq!(slot, expected_clock.slot);
    assert_eq!(epoch, expected_clock.epoch);
    assert_eq!(epoch_start_ts as i64, expected_clock.epoch_start_timestamp);
    assert_eq!(lse, expected_clock.leader_schedule_epoch);
    assert_eq!(ts as i64, expected_clock.unix_timestamp);
}

fn parse_logs(logs: &[String]) -> (u64, u64, u64, u64, u64) {
    let mut vals = [0u64; 5];
    for log in logs {
        if let Some(v) = log.strip_prefix("Program log: ") {
            if let Some((_, n)) = v.split_once(": ") {
                if let Ok(num) = n.parse() {
                    match v.split(':').next() {
                        Some("Slot") => vals[0] = num,
                        Some("Epoch") => vals[1] = num,
                        Some("Epoch Start Timestamp") => vals[2] = num,
                        Some("Leader Schedule Epoch") => vals[3] = num,
                        Some("Timestamp") => vals[4] = num,
                        _ => {}
                    }
                }
            }
        }
    }
    (vals[0], vals[1], vals[2], vals[3], vals[4])
}
