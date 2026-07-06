use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_clock::Clock;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_sysvar_id::{SysvarId, ID as SYSVAR};

#[test]
fn profile_sysvar_program() {
    let mut seashell = Seashell::new();
    let out_dir = try_find_workspace_root()
        .unwrap()
        .join("programs/sysvar/target/deploy");
    unsafe { std::env::set_var("SBF_OUT_DIR", out_dir.to_str().unwrap()) }

    let program_id = Pubkey::new_unique();
    seashell
        .load_program_from_environment("sysvar", program_id)
        .unwrap();

    let expected_clock = Clock {
        slot: 1,
        epoch_start_timestamp: 2,
        epoch: 3,
        leader_schedule_epoch: 4,
        unix_timestamp: 5,
    };
    seashell.set_account(
        Clock::id(),
        Account {
            lamports: 1000,
            data: bincode::serialize(&expected_clock).unwrap(),
            owner: SYSVAR,
            executable: false,
            rent_epoch: 0,
        },
    );

    let ixn = Instruction {
        program_id,
        accounts: Vec::new(),
        data: Vec::new(),
    };

    let result = seashell.profile_instruction(ixn);
    assert!(result.error.is_none(), "unexpected error: {:?}", result.error);
    let profiler = seashell.profiler.clone().expect("profiler populated");

    assert_eq!(
        profiler.total_self_cu(),
        result.compute_units_consumed,
        "profiler self-CU ({}) must equal compute_units_consumed ({})",
        profiler.total_self_cu(),
        result.compute_units_consumed,
    );
    assert!(profiler.total_self_cu() > 0, "expected some CU attributed");

    let sym = seashell
        .symbolicator(&program_id)
        .cloned()
        .unwrap_or_default();

    eprintln!(
        "\n=== sysvar profile: {} CU across {} nodes ===",
        result.compute_units_consumed,
        profiler.nodes.len(),
    );
    for (i, n) in profiler.nodes.iter().enumerate() {
        eprintln!(
            "  node[{i}] {} parent={:?} self_cu={} children={}",
            sym.label(n.key),
            n.parent,
            n.self_cu,
            n.children.len(),
        );
    }

    eprintln!("\n=== folded stacks (collapsed; feed to a flamegraph) ===");
    for line in sym.folded_inlined(&profiler) {
        eprintln!("{line}");
    }

    let svg_path = seashell.write_svg();
    let svg_len = std::fs::metadata(&svg_path).unwrap().len();
    assert!(svg_len > 0, "flamegraph SVG should be non-empty");
    eprintln!("\nwrote flamegraph: {} ({svg_len} bytes)", svg_path.display());
    seashell.clear_profiler();
    assert!(seashell.profiler.is_none());
}
