use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_clock::Clock;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sbpf::profiler::FrameKey;
use solana_sysvar_id::{SysvarId, ID as SYSVAR};

#[test]
fn profile_cpi_caller_into_sysvar() {
    let mut seashell = Seashell::new();
    let root = try_find_workspace_root().unwrap();

    let caller_id = Pubkey::new_unique();
    let callee_id = Pubkey::new_unique();

    for (dir, name, id) in [
        ("programs/cpi-caller/target/deploy", "cpi_caller", caller_id),
        ("programs/sysvar/target/deploy", "sysvar", callee_id),
    ] {
        unsafe { std::env::set_var("SBF_OUT_DIR", root.join(dir).to_str().unwrap()) }
        seashell.load_program_from_environment(name, id).unwrap();
    }

    seashell.set_account(
        Clock::id(),
        Account {
            lamports: 1000,
            data: bincode::serialize(&Clock::default()).unwrap(),
            owner: SYSVAR,
            executable: false,
            rent_epoch: 0,
        },
    );

    let ixn = Instruction {
        program_id: caller_id,
        accounts: vec![AccountMeta::new_readonly(callee_id, false)],
        data: Vec::new(),
    };

    let result = seashell.profile_instruction(ixn);
    assert!(result.error.is_none(), "unexpected error: {:?}", result.error);
    let profiler = seashell.profiler.clone().expect("profiler populated");

    assert_eq!(
        profiler.total_self_cu(),
        result.compute_units_consumed,
        "profiler self-CU must equal consumed CU across CPI",
    );

    assert_eq!(
        profiler.nodes[0].key,
        FrameKey::Program(caller_id.to_bytes()),
        "root must be the caller program frame",
    );
    let callee_node = profiler
        .nodes
        .iter()
        .position(|n| n.key == FrameKey::Program(callee_id.to_bytes()))
        .expect("callee program frame present");
    let invoke_syscall = profiler.nodes[callee_node].parent.unwrap();
    assert!(
        matches!(profiler.nodes[invoke_syscall].key, FrameKey::Syscall(_)),
        "callee program hangs under the invoke syscall",
    );
    assert!(
        profiler.nodes[invoke_syscall].self_cu > 0,
        "invoke frame keeps its own (non-callee) overhead",
    );
    let callee_subtree_cu: u64 = profiler
        .nodes
        .iter()
        .enumerate()
        .filter(|(idx, _)| {
            let mut p = Some(*idx);
            while let Some(i) = p {
                if i == callee_node {
                    return true;
                }
                p = profiler.nodes[i].parent;
            }
            false
        })
        .map(|(_, n)| n.self_cu)
        .sum();
    assert!(
        callee_subtree_cu > 0,
        "callee subtree carries the callee's CU",
    );

    let fallback = seashell::symbolicate::Symbolicator::new();
    let syms = seashell.program_symbolicators(&fallback);
    let folded = syms.folded_inlined(&profiler);
    eprintln!(
        "\n=== cpi profile: {} CU across {} nodes ===",
        result.compute_units_consumed,
        profiler.nodes.len(),
    );
    for line in &folded {
        eprintln!("{line}");
    }
    let caller_symbolized = folded
        .iter()
        .any(|l| l.contains("cpi_caller::entrypoint::process_instruction"));
    let callee_symbolized = folded.iter().any(|l| {
        l.contains(&callee_id.to_string()) && l.contains("Clock as pinocchio::sysvars::Sysvar")
    });
    assert!(caller_symbolized, "caller frames symbolize with caller DWARF");
    assert!(callee_symbolized, "callee frames symbolize with callee DWARF");

    let out_dir = root.join("target/flamegraphs");
    std::fs::create_dir_all(&out_dir).unwrap();
    let svg = std::fs::File::create(out_dir.join("cpi_caller.svg")).unwrap();
    syms.render_svg(&profiler, "cpi-caller -> sysvar - CU flamegraph", svg)
        .expect("render flamegraph");
}
