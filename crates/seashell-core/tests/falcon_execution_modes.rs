//! Falcon-512 verification (blueshift-gg/solana-falcon512 at 0ee114f, example program built with
//! `cargo build-sbf --arch v3`) under every execution mode.
//!
//! The time measured is the program's execution alone: the time agave spends inside
//! `vm.execute_program`, which it records per program in whole microseconds. Everything around
//! it (account loading, serialization, VM creation, JIT compilation) is left out.
//!
//! Run with:
//!
//! ```text
//! cargo test --release -p seashell --test falcon_execution_modes --no-run
//! RUST_LOG=error taskset -c 12 <test binary> --ignored --nocapture
//! ```

use std::sync::atomic::Ordering;

use seashell::{ExecutionMode, Seashell};
use solana_instruction::Instruction;
use solana_program_runtime::program_cache::entry::{ProgramCacheEntry, ProgramCacheEntryType};
use solana_pubkey::Pubkey;

const PROGRAM: &[u8] = include_bytes!("fixtures/falcon512-blueshift/falcon512_program.so");
const SIGNATURE: &[u8] = include_bytes!("fixtures/falcon512-blueshift/sample_sig.bin");
const MESSAGE: &[u8] = b"deterministic falcon-512 verify benchmark";

/// Instruction data mode 0: SHAKE256 with the pubkey embedded in the program.
const SHAKE_EMBEDDED_PUBKEY: u8 = 0;

const MODES: [(&str, ExecutionMode); 4] = [
    ("legacy interp", ExecutionMode::Interpreted),
    ("legacy jit", ExecutionMode::Jit),
    ("parrot interp", ExecutionMode::ParrotInterpreted),
    ("parrot jit", ExecutionMode::ParrotJit),
];

/// The modes are measured in rounds, one after another, so that slow drift of the machine
/// spreads over all of them.
const ROUNDS: usize = 10;
/// Unmeasured executions after every mode switch. The first one also recompiles the program
/// when the mode needs the other JIT.
const WARMUP_EXECUTIONS: usize = 20;
const MEASURED_EXECUTIONS_PER_ROUND: usize = 200;

/// Total (JIT, interpreter) execution time of the program so far, in microseconds, and how many
/// executions each has had.
fn execution_totals(entry: &ProgramCacheEntry) -> [(u64, u64); 2] {
    let load = |counter: &std::sync::atomic::AtomicU64| counter.load(Ordering::Relaxed);
    [
        (load(&entry.stats.total_jit_execution_time_us), load(&entry.stats.jit_invocations)),
        (
            load(&entry.stats.total_interpretation_time_us),
            load(&entry.stats.interpreted_invocations),
        ),
    ]
}

/// How long each JIT takes to compile the program, as loaded by seashell (agave's VM config).
#[test]
#[ignore = "benchmark"]
fn falcon_jit_compile_times() {
    const WARMUP_COMPILATIONS: usize = 20;
    const MEASURED_COMPILATIONS: usize = 500;

    let mut seashell = Seashell::new();
    let program_id = Pubkey::new_unique();
    seashell.load_program_from_bytes(program_id, PROGRAM);
    let entry = seashell
        .accounts_db
        .programs
        .get_entry_for_tests(&program_id)
        .expect("the program is loaded")
        .clone();
    let ProgramCacheEntryType::Loaded(executable) = &entry.program else {
        panic!("the program is not a loaded executable");
    };

    // The two take turns, so that slow drift of the machine spreads over both.
    let mut compile_ns: [Vec<u64>; 2] = Default::default();
    for compilation in 0..WARMUP_COMPILATIONS + MEASURED_COMPILATIONS {
        for (index, parrot) in [false, true].into_iter().enumerate() {
            let start = std::time::Instant::now();
            if parrot {
                executable.parrot_compile().unwrap();
            } else {
                executable.jit_compile().unwrap();
            }
            let elapsed = start.elapsed().as_nanos() as u64;
            let compiled = executable.get_compiled_program().expect("compiled");
            assert_eq!(compiled.is_parrot(), parrot);
            if compilation >= WARMUP_COMPILATIONS {
                compile_ns[index].push(elapsed);
            }
        }
    }

    println!("falcon-512 program, {MEASURED_COMPILATIONS} compilations per mode");
    println!();
    println!("| mode | p50 compile (us) | avg compile (us) | vs legacy jit |");
    println!("|---|---:|---:|---:|");
    let mut legacy_jit_p50 = 0.0;
    for (index, name) in ["legacy jit", "parrot jit"].into_iter().enumerate() {
        let samples = &mut compile_ns[index];
        samples.sort_unstable();
        let p50 = samples[samples.len() / 2] as f64 / 1000.0;
        let avg = samples.iter().sum::<u64>() as f64 / samples.len() as f64 / 1000.0;
        if index == 0 {
            legacy_jit_p50 = p50;
        }
        println!("| {name} | {p50:.1} | {avg:.1} | {:.2}x |", legacy_jit_p50 / p50);
    }
}

#[test]
#[ignore = "benchmark"]
fn falcon_execution_modes() {
    let mut seashell = Seashell::new();
    let program_id = Pubkey::new_unique();
    seashell.load_program_from_bytes(program_id, PROGRAM);
    let entry = seashell
        .accounts_db
        .programs
        .get_entry_for_tests(&program_id)
        .expect("the program is loaded")
        .clone();
    let ProgramCacheEntryType::Loaded(executable) = &entry.program else {
        panic!("the program is not a loaded executable");
    };

    let mut data = vec![SHAKE_EMBEDDED_PUBKEY];
    data.extend_from_slice(SIGNATURE);
    data.extend_from_slice(MESSAGE);
    let instruction = Instruction { program_id, accounts: Vec::new(), data };

    let mut execution_us: [Vec<u64>; MODES.len()] = Default::default();
    let mut compute_units = [0; MODES.len()];
    for _ in 0..ROUNDS {
        for (index, (name, mode)) in MODES.into_iter().enumerate() {
            seashell.execution_mode = mode;
            let is_jit = matches!(mode, ExecutionMode::Jit | ExecutionMode::ParrotJit);
            for execution in 0..WARMUP_EXECUTIONS + MEASURED_EXECUTIONS_PER_ROUND {
                let before = execution_totals(&entry);
                let result = seashell.process_instruction(instruction.clone());
                let after = execution_totals(&entry);
                assert!(result.error.is_none(), "{name}: {:?}", result.error);
                compute_units[index] = result.compute_units_consumed;

                // The execution must have been counted on the side the mode belongs to, once.
                let [jit, interpreted] = [0, 1].map(|side| {
                    (after[side].0 - before[side].0, after[side].1 - before[side].1)
                });
                let (us, other) = if is_jit { (jit, interpreted) } else { (interpreted, jit) };
                assert_eq!((us.1, other.1), (1, 0), "{name} ran in the wrong kind of engine");
                if is_jit {
                    let compiled = executable.get_compiled_program().expect("compiled");
                    assert_eq!(compiled.is_parrot(), mode == ExecutionMode::ParrotJit, "{name}");
                }
                if execution >= WARMUP_EXECUTIONS {
                    execution_us[index].push(us.0);
                }
            }
        }
    }

    assert!(compute_units.iter().all(|&units| units == compute_units[0]), "{compute_units:?}");
    println!(
        "falcon-512 verify, {} CU, {} executions per mode",
        compute_units[0],
        ROUNDS * MEASURED_EXECUTIONS_PER_ROUND
    );
    println!();
    println!("| mode | p50 exec (us) | avg exec (us) | vs legacy interp |");
    println!("|---|---:|---:|---:|");
    let mut legacy_interp_p50 = 0.0;
    for (index, (name, _)) in MODES.into_iter().enumerate() {
        let samples = &mut execution_us[index];
        samples.sort_unstable();
        let p50 = samples[samples.len() / 2] as f64;
        let avg = samples.iter().sum::<u64>() as f64 / samples.len() as f64;
        if index == 0 {
            legacy_interp_p50 = p50;
        }
        println!("| {name} | {p50:.0} | {avg:.1} | {:.2}x |", legacy_interp_p50 / p50);
    }
}
