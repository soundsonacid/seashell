//! Replays blocks of cluster traffic on account state taken from a snapshot, under every
//! execution mode, and compares the time spent executing SBF programs.
//!
//! ```text
//! seashell-replay extract <blocks dir> <accounts file> <full snapshot> [<incremental snapshot>]
//! seashell-replay bench   <blocks dir> <accounts file> [<measured rounds>]
//! seashell-replay fidelity <blocks dir> <accounts file>
//! ```
//!
//! `extract` reads the snapshot archives once and keeps the accounts the blocks name. `bench`
//! works from that file alone.

mod blocks;
mod diagnose;
mod fidelity;
mod state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use blocks::Block;
use seashell::{ExecutionMode, Seashell};
use solana_sbpf::codegen::call_dispatch::CallDispatchCounts;
use solana_sbpf::timing::VERSIONS;
use solana_instruction_error::InstructionError;
use solana_account::{ReadableAccount, WritableAccount};
use solana_pubkey::Pubkey;

const MODES: [(&str, ExecutionMode); 4] = [
    ("legacy interp", ExecutionMode::Interpreted),
    ("legacy jit", ExecutionMode::Jit),
    ("parrot interp", ExecutionMode::ParrotInterpreted),
    ("parrot jit", ExecutionMode::ParrotJit),
];

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match arguments.as_slice() {
        ["extract", blocks, accounts, archives @ ..] if !archives.is_empty() => {
            extract(Path::new(blocks), Path::new(accounts), archives)
        }
        ["fidelity", blocks, accounts] => {
            let blocks = blocks::read_blocks(Path::new(blocks));
            let accounts = state::read_accounts(Path::new(accounts));
            let (mut seashell, _) = state::seashell_from_accounts(&accounts);
            let start = state::Start::of(&seashell);
            fidelity::report(&mut seashell, &start, &blocks);
        }
        ["diagnose", blocks, accounts] => {
            let blocks = blocks::read_blocks(Path::new(blocks));
            let accounts = state::read_accounts(Path::new(accounts));
            let (mut seashell, _) = state::seashell_from_accounts(&accounts);
            let start = state::Start::of(&seashell);
            diagnose::report(&mut seashell, &start, &blocks);
        }
        ["call-dispatch-experiment", blocks, accounts, rounds] => call_dispatch_experiment(
            Path::new(blocks),
            Path::new(accounts),
            rounds.parse().expect("the number of measured rounds"),
        ),
        ["programs", accounts] => {
            let accounts = state::read_accounts(Path::new(accounts));
            println!("address\tversion\tloader\tdeployment_slot");
            for (address, version, loader, slot) in state::program_deployments(&accounts) {
                let slot = slot.map_or(String::new(), |slot| slot.to_string());
                println!("{address}\t{version}\t{loader}\t{slot}");
            }
        }
        ["bench", blocks, accounts] => bench(Path::new(blocks), Path::new(accounts), 5),
        ["bench", blocks, accounts, rounds] => bench(
            Path::new(blocks),
            Path::new(accounts),
            rounds.parse().expect("the number of measured rounds"),
        ),
        _ => {
            eprintln!(
                "usage:\n  seashell-replay extract <blocks dir> <accounts file> <full snapshot> \
                 [<incremental snapshot>]\n  seashell-replay bench <blocks dir> <accounts file> \
                 [<measured rounds>]\n  seashell-replay fidelity <blocks dir> <accounts file>"
            );
            std::process::exit(2);
        }
    }
}

fn extract(blocks_directory: &Path, accounts_file: &Path, archives: &[&str]) {
    let blocks = blocks::read_blocks(blocks_directory);
    let addresses = blocks::addresses(&blocks);
    let wanted = state::wanted_addresses(&addresses);
    eprintln!(
        "{} blocks name {} addresses, looking for {} accounts",
        blocks.len(),
        addresses.len(),
        wanted.len()
    );

    let mut found = HashMap::new();
    for archive in archives {
        let start = Instant::now();
        let progress = seashell_snapshot::extract_accounts(
            &PathBuf::from(archive),
            &wanted,
            &mut found,
            |progress| {
                eprintln!(
                    "  {:>5} GiB, {:>11} account entries, {:>7} matched, {:.0} s",
                    progress.uncompressed_bytes >> 30,
                    progress.accounts_seen,
                    progress.accounts_matched,
                    start.elapsed().as_secs_f64()
                )
            },
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let seconds = start.elapsed().as_secs_f64();
        eprintln!(
            "{archive}: {:.1} GiB uncompressed, {} account entries, {} matched, {seconds:.0} s \
             ({:.2} GiB/s)",
            progress.uncompressed_bytes as f64 / (1u64 << 30) as f64,
            progress.accounts_seen,
            progress.accounts_matched,
            progress.uncompressed_bytes as f64 / (1u64 << 30) as f64 / seconds,
        );
    }
    eprintln!("{} of {} accounts exist in the snapshot", found.len(), wanted.len());
    state::write_accounts(accounts_file, found);
}

/// What one transaction did in one run.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Outcome {
    succeeded: bool,
    compute_units_consumed: u64,
    syscalls: u64,
    program_runs: u64,
}

/// One pass over the blocks. Every vector has one entry per transaction.
struct Run {
    outcomes: Vec<Outcome>,
    /// Time executing program instructions, in nanoseconds.
    instruction_ns: Vec<u64>,
    /// Time inside syscalls, in nanoseconds.
    syscall_ns: Vec<u64>,
    errors: Vec<Option<InstructionError>>,
    /// Calls and nanoseconds per syscall, over the whole pass.
    by_syscall: HashMap<&'static str, (u64, u64)>,
    /// `call imm` instructions that parrot resolved at run time, per transaction: calls to the
    /// program's own functions, and syscalls. Zero in the legacy modes, which do not count.
    dispatched: Vec<CallDispatchCounts>,
    /// Instruction time in milliseconds and program runs, by the SBPF version of the program
    /// executing, over the whole pass.
    by_version: [(f64, u64); VERSIONS],
}

/// What timing one syscall adds, in nanoseconds: to the syscall's time and to the instruction
/// time around it. Measured once, then taken out of every figure.
#[derive(Clone, Copy)]
struct TimerOverhead {
    in_syscall: f64,
    in_instructions: f64,
}

impl TimerOverhead {
    fn measure() -> Self {
        let (in_syscall, in_instructions) = solana_sbpf::timing::syscall_timer_overhead();
        TimerOverhead { in_syscall, in_instructions }
    }

    /// `ns` without the share of timing `syscalls` syscalls that costs `per_syscall` each.
    fn without(ns: u64, syscalls: u64, per_syscall: f64) -> u64 {
        ns.saturating_sub((syscalls as f64 * per_syscall) as u64)
    }
}

/// Runs every transaction of `blocks` in order, from the snapshot's state.
fn run_blocks(
    seashell: &mut Seashell,
    start: &state::Start,
    blocks: &[Block],
    overhead: TimerOverhead,
) -> Run {
    start.restore_accounts(seashell);
    let mut outcomes = Vec::new();
    let mut instruction_ns = Vec::new();
    let mut syscall_ns = Vec::new();
    let mut errors = Vec::new();
    let syscalls_before = solana_sbpf::timing::syscall_times();
    let versions_before = solana_sbpf::timing::version_times();
    let mut dispatched = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
        blocks::enter_block(seashell, blocks, block_index, start);
        for transaction in &block.transactions {
            // The fee is paid whether or not the transaction succeeds.
            let fee_payer = *transaction.transaction.message().fee_payer();
            let mut account = seashell.accounts_db.account_maybe(&fee_payer).unwrap_or_default();
            account.set_lamports(account.lamports().saturating_sub(transaction.fee));
            seashell.accounts_db.set_account(fee_payer, account);

            let limits = blocks::limits(seashell, &transaction.transaction);
            let dispatched_before = solana_sbpf::codegen::call_dispatch::call_dispatch_counts();
            let result = seashell.process_transaction(&transaction.transaction, limits);
            dispatched.push(
                solana_sbpf::codegen::call_dispatch::call_dispatch_counts()
                    .since(&dispatched_before),
            );
            outcomes.push(Outcome {
                succeeded: result.error.is_none(),
                compute_units_consumed: result.compute_units_consumed,
                syscalls: result.syscalls,
                program_runs: result.program_runs,
            });
            instruction_ns.push(TimerOverhead::without(
                result.instruction_time.as_nanos() as u64,
                result.syscalls,
                overhead.in_instructions,
            ));
            syscall_ns.push(TimerOverhead::without(
                result.syscall_time.as_nanos() as u64,
                result.syscalls,
                overhead.in_syscall,
            ));
            errors.push(result.error.map(|(_, error)| error));
        }
    }

    let versions_after = solana_sbpf::timing::version_times();
    let by_version: [(f64, u64); VERSIONS] = std::array::from_fn(|version| {
        let (before, after) = (versions_before[version], versions_after[version]);
        let ns = TimerOverhead::without(
            after.instructions_ns - before.instructions_ns,
            after.syscalls - before.syscalls,
            overhead.in_instructions,
        );
        (ns as f64 / 1e6, after.program_runs - before.program_runs)
    });
    let mut by_syscall: HashMap<&'static str, (u64, u64)> = HashMap::new();
    for after in solana_sbpf::timing::syscall_times() {
        let before = syscalls_before.iter().find(|before| before.name == after.name);
        let calls = after.calls - before.map_or(0, |before| before.calls);
        let ns = after.ns - before.map_or(0, |before| before.ns);
        if calls > 0 {
            by_syscall.insert(
                after.name,
                (calls, TimerOverhead::without(ns, calls, overhead.in_syscall)),
            );
        }
    }
    Run {
        outcomes,
        instruction_ns,
        syscall_ns,
        errors,
        by_syscall,
        dispatched,
        by_version,
    }
}

fn bench(blocks_directory: &Path, accounts_file: &Path, measured_rounds: usize) {
    let blocks = blocks::read_blocks(blocks_directory);
    let accounts = state::read_accounts(accounts_file);
    let (mut seashell, report) = state::seashell_from_accounts(&accounts);
    let start = state::Start::of(&seashell);

    let cluster: Vec<&blocks::BlockTransaction> =
        blocks.iter().flat_map(|block| &block.transactions).collect();
    println!(
        "slots {}..={}: {} blocks, {} transactions, {} compute units on the cluster",
        blocks.first().map_or(0, |block| block.slot),
        blocks.last().map_or(0, |block| block.slot),
        blocks.len(),
        cluster.len(),
        cluster.iter().filter_map(|transaction| transaction.compute_units_consumed).sum::<u64>(),
    );
    println!(
        "state: {} accounts, {} of {} known features active, {} programs loaded, {} not loaded",
        report.accounts,
        report.active_features,
        report.known_features,
        report.programs_loaded,
        report.programs_failed.len(),
    );
    for (address, why) in &report.programs_failed {
        println!("  not loaded: {address}: {why}");
    }

    // Round 0 is a warmup: it also compiles each program for the mode on first use. The modes
    // take turns within a round, so that slow drift of the machine spreads over all of them.
    let mut reference: Option<Vec<Outcome>> = None;
    let mut reference_errors = Vec::new();
    let mut differing_outcomes = [0usize; MODES.len()];
    let overhead = TimerOverhead::measure();
    println!(
        "timing a syscall costs {:.1} ns inside the syscall and {:.1} ns around it; both are \
         taken out of the figures below",
        overhead.in_syscall, overhead.in_instructions
    );
    let mut instruction_ns: [Vec<Vec<u64>>; MODES.len()] = Default::default();
    let mut syscall_ns: [Vec<Vec<u64>>; MODES.len()] = Default::default();
    let mut by_syscall: [Vec<HashMap<&'static str, (u64, u64)>>; MODES.len()] = Default::default();
    let mut by_version: [Vec<[(f64, u64); VERSIONS]>; MODES.len()] = Default::default();
    // What parrot resolved at run time, per transaction. Only the parrot modes count it.
    let mut dispatched = Vec::new();
    for round in 0..=measured_rounds {
        for (index, (_, mode)) in MODES.into_iter().enumerate() {
            seashell.execution_mode = mode;
            // An unmeasured pass first: switching between the JITs recompiles every program on
            // first use, which leaves its code cold.
            run_blocks(&mut seashell, &start, &blocks, overhead);
            let run = run_blocks(&mut seashell, &start, &blocks, overhead);
            if mode == ExecutionMode::ParrotJit {
                dispatched = run.dispatched.clone();
            }
            if reference.is_none() {
                reference_errors = run.errors;
            }
            let reference = reference.get_or_insert_with(|| run.outcomes.clone());
            differing_outcomes[index] = differing_outcomes[index].max(
                run.outcomes
                    .iter()
                    .zip(reference.iter())
                    .filter(|(ours, theirs)| ours != theirs)
                    .count(),
            );
            if round > 0 {
                instruction_ns[index].push(run.instruction_ns);
                syscall_ns[index].push(run.syscall_ns);
                by_syscall[index].push(run.by_syscall);
                by_version[index].push(run.by_version);
            }
        }
        eprintln!("round {round} of {measured_rounds} done");
    }
    let reference = reference.expect("at least the warmup round ran");
    println!(
        "programs by SBPF version: {:?}",
        report.programs_by_version
    );

    // How close the replay is to what the cluster did.
    let same_status: Vec<bool> = reference
        .iter()
        .zip(&cluster)
        .map(|(ours, theirs)| ours.succeeded == theirs.succeeded)
        .collect();
    let faithful: Vec<bool> = reference
        .iter()
        .zip(&cluster)
        .map(|(ours, theirs)| {
            ours.succeeded == theirs.succeeded
                && theirs.compute_units_consumed == Some(ours.compute_units_consumed)
        })
        .collect();
    let count = |flags: &[bool]| flags.iter().filter(|&&flag| flag).count();
    println!(
        "replay against the cluster: {} of {} transactions end the same way (success or failure), \
         {} of those with the same compute units",
        count(&same_status),
        cluster.len(),
        count(&faithful),
    );
    println!(
        "compute units replayed: {} in all, {} in the transactions that match the cluster",
        reference.iter().map(|outcome| outcome.compute_units_consumed).sum::<u64>(),
        reference
            .iter()
            .zip(&faithful)
            .filter(|(_, &faithful)| faithful)
            .map(|(outcome, _)| outcome.compute_units_consumed)
            .sum::<u64>(),
    );
    // Why the replay fails transactions that succeeded on the cluster.
    let mut replay_only_failures: HashMap<String, usize> = HashMap::new();
    for (error, theirs) in reference_errors.iter().zip(&cluster) {
        if let (Some(error), true) = (error, theirs.succeeded) {
            *replay_only_failures.entry(format!("{error:?}")).or_default() += 1;
        }
    }
    let mut replay_only_failures: Vec<_> = replay_only_failures.into_iter().collect();
    replay_only_failures.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (error, count) in replay_only_failures.iter().take(8) {
        println!("  succeeded on the cluster, fails in the replay: {count} with {error}");
    }
    for (index, (name, _)) in MODES.into_iter().enumerate() {
        println!(
            "{name}: {} transactions differ from legacy interp in outcome, compute units or \
             number of syscalls",
            differing_outcomes[index]
        );
    }

    println!(
        "syscalls: {} in all, {} in the transactions that match the cluster",
        reference.iter().map(|outcome| outcome.syscalls).sum::<u64>(),
        reference
            .iter()
            .zip(&faithful)
            .filter(|(_, &faithful)| faithful)
            .map(|(outcome, _)| outcome.syscalls)
            .sum::<u64>(),
    );

    // Total program execution is the time inside the top-level programs' runs: instructions
    // plus syscalls.
    let total_ns: [Vec<Vec<u64>>; MODES.len()] = std::array::from_fn(|mode| {
        instruction_ns[mode]
            .iter()
            .zip(&syscall_ns[mode])
            .map(|(instructions, syscalls)| {
                instructions.iter().zip(syscalls).map(|(a, b)| a + b).collect()
            })
            .collect()
    });
    let everything = vec![true; cluster.len()];
    let rounds = measured_rounds;
    print_table("total program execution (instructions + syscalls)", &total_ns, &everything, rounds);
    print_table("instructions only", &instruction_ns, &everything, rounds);
    print_table("syscalls only", &syscall_ns, &everything, rounds);
    print_syscalls(&by_syscall);
    print_cost_model(&reference, &instruction_ns);
    print_programs(&cluster, &instruction_ns);
    print_by_version(&by_version);
    print_near_call_attribution(&dispatched, &instruction_ns);
}

const LEGACY_JIT: usize = 1;
const PARROT_JIT: usize = 3;

/// Instruction time by the SBPF version of the program that is executing, per mode, as the
/// median over the rounds.
fn print_by_version(by_version: &[Vec<[(f64, u64); VERSIONS]>; MODES.len()]) {
    println!();
    println!("instruction time by SBPF version of the executing program (ms, median over the rounds)");
    println!();
    println!(
        "| programs' version | program runs | legacy interp | legacy jit | parrot interp | parrot \
         jit | parrot jit vs legacy jit | parrot jit minus legacy jit (ms) |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    let median_ms = |mode: usize, version: usize| {
        let mut samples: Vec<f64> = by_version[mode].iter().map(|round| round[version].0).collect();
        samples.sort_by(f64::total_cmp);
        samples[samples.len() / 2]
    };
    let mut totals = [0.0; MODES.len()];
    for version in 0..VERSIONS {
        let runs = by_version[0][0][version].1;
        if runs == 0 {
            continue;
        }
        let ms: [f64; MODES.len()] = std::array::from_fn(|mode| median_ms(mode, version));
        for mode in 0..MODES.len() {
            totals[mode] += ms[mode];
        }
        println!(
            "| V{version} | {runs} | {:.1} | {:.1} | {:.1} | {:.1} | {:.2}x | {:+.1} |",
            ms[0],
            ms[1],
            ms[2],
            ms[3],
            ms[LEGACY_JIT] / ms[PARROT_JIT],
            ms[PARROT_JIT] - ms[LEGACY_JIT],
        );
    }
    println!(
        "| all | | {:.1} | {:.1} | {:.1} | {:.1} | {:.2}x | {:+.1} |",
        totals[0],
        totals[1],
        totals[2],
        totals[3],
        totals[LEGACY_JIT] / totals[PARROT_JIT],
        totals[PARROT_JIT] - totals[LEGACY_JIT],
    );
}

/// Parrot jit against legacy jit, instruction time per transaction (median over the rounds),
/// grouped by what parrot had to resolve at run time in the transaction.
fn print_near_call_attribution(
    dispatched: &[CallDispatchCounts],
    instruction_ns: &[Vec<Vec<u64>>; MODES.len()],
) {
    let median_ms = |mode: usize, transaction: usize| {
        let mut samples: Vec<u64> =
            instruction_ns[mode].iter().map(|round| round[transaction]).collect();
        samples.sort_unstable();
        samples[samples.len() / 2] as f64 / 1e6
    };
    let legacy: Vec<f64> = (0..dispatched.len()).map(|tx| median_ms(LEGACY_JIT, tx)).collect();
    let parrot: Vec<f64> = (0..dispatched.len()).map(|tx| median_ms(PARROT_JIT, tx)).collect();
    let print_groups = |title: &str, groups: &[(&str, &dyn Fn(&CallDispatchCounts) -> bool)]| {
        println!();
        println!("{title}");
        println!();
        println!(
            "| transactions | txs | near calls resolved at run time | syscalls from pre-v3 \
             programs | syscalls from v3 programs | legacy jit (ms) | parrot jit (ms) | parrot jit \
             vs legacy jit | gap (ms) | gap per near call (ns) |"
        );
        println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
        for (label, belongs) in groups {
            let members: Vec<usize> =
                (0..dispatched.len()).filter(|&tx| belongs(&dispatched[tx])).collect();
            let sum = |field: &dyn Fn(&CallDispatchCounts) -> u64| -> u64 {
                members.iter().map(|&tx| field(&dispatched[tx])).sum()
            };
            let near_calls = sum(&|counts| counts.internal_calls);
            let legacy_ms: f64 = members.iter().map(|&tx| legacy[tx]).sum();
            let parrot_ms: f64 = members.iter().map(|&tx| parrot[tx]).sum();
            let per_call = if near_calls == 0 {
                String::new()
            } else {
                format!("{:.1}", (parrot_ms - legacy_ms) * 1e6 / near_calls as f64)
            };
            println!(
                "| {label} | {} | {near_calls} | {} | {} | {legacy_ms:.1} | {parrot_ms:.1} | \
                 {:.2}x | {:+.1} | {per_call} |",
                members.len(),
                sum(&|counts| counts.syscalls),
                sum(&|counts| counts.static_syscalls),
                legacy_ms / parrot_ms,
                parrot_ms - legacy_ms,
            );
        }
    };

    print_groups(
        "parrot jit against legacy jit, instruction time, by what the transaction's programs call",
        &[
            ("no calls and no syscalls", &|c| {
                c.internal_calls == 0 && c.syscalls == 0 && c.static_syscalls == 0
            }),
            ("v3 programs only (syscalls, near calls are relative)", &|c| {
                c.internal_calls == 0 && c.syscalls == 0 && c.static_syscalls > 0
            }),
            ("pre-v3 programs that make syscalls but no near calls", &|c| {
                c.internal_calls == 0 && c.syscalls > 0
            }),
            ("pre-v3 programs that make near calls", &|c| c.internal_calls > 0),
            ("all", &|_| true),
        ],
    );
    print_groups(
        "the same, by how many near calls the transaction makes",
        &[
            ("0", &|c| c.internal_calls == 0),
            ("1 to 10", &|c| (1..=10).contains(&c.internal_calls)),
            ("11 to 100", &|c| (11..=100).contains(&c.internal_calls)),
            ("101 to 1,000", &|c| (101..=1_000).contains(&c.internal_calls)),
            ("1,001 to 10,000", &|c| (1_001..=10_000).contains(&c.internal_calls)),
            ("over 10,000", &|c| c.internal_calls > 10_000),
        ],
    );
}

/// Splits each mode's instruction time into a cost per compute unit, per syscall and per
/// program run, by least squares over the transactions:
///
/// ```text
/// instruction time of a transaction = a * compute units + b * syscalls + c * program runs
/// ```
///
/// A transaction's time is its median over the rounds. Compute units stand in for instructions
/// executed, which they are apart from what syscalls charge.
fn print_cost_model(outcomes: &[Outcome], instruction_ns: &[Vec<Vec<u64>>; MODES.len()]) {
    println!();
    println!("instruction time as cost per compute unit, per syscall and per program run");
    println!();
    println!(
        "| mode | ns per CU | ns per syscall | ns per program run | share explained (R²) |"
    );
    println!("|---|---:|---:|---:|---:|");
    let features: Vec<[f64; 3]> = outcomes
        .iter()
        .map(|outcome| {
            [
                outcome.compute_units_consumed as f64,
                outcome.syscalls as f64,
                outcome.program_runs as f64,
            ]
        })
        .collect();
    for (mode, (name, _)) in MODES.into_iter().enumerate() {
        let times: Vec<f64> = (0..outcomes.len())
            .map(|transaction| {
                let mut samples: Vec<u64> =
                    instruction_ns[mode].iter().map(|round| round[transaction]).collect();
                samples.sort_unstable();
                samples[samples.len() / 2] as f64
            })
            .collect();
        let coefficients = least_squares(&features, &times);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        let (mut residual, mut total) = (0.0, 0.0);
        for (row, time) in features.iter().zip(&times) {
            let predicted: f64 = row.iter().zip(&coefficients).map(|(x, c)| x * c).sum();
            residual += (time - predicted).powi(2);
            total += (time - mean).powi(2);
        }
        println!(
            "| {name} | {:.2} | {:.0} | {:.0} | {:.3} |",
            coefficients[0],
            coefficients[1],
            coefficients[2],
            1.0 - residual / total
        );
    }
    println!(
        "| (per transaction, mean) | {:.0} CU | {:.1} syscalls | {:.2} runs | |",
        features.iter().map(|row| row[0]).sum::<f64>() / features.len() as f64,
        features.iter().map(|row| row[1]).sum::<f64>() / features.len() as f64,
        features.iter().map(|row| row[2]).sum::<f64>() / features.len() as f64,
    );
}

/// The `c` that minimizes the sum over rows of `(y - x . c)^2`, from the normal equations.
fn least_squares<const N: usize>(rows: &[[f64; N]], y: &[f64]) -> [f64; N] {
    // [X'X | X'y], then Gauss-Jordan elimination.
    let mut system = vec![vec![0.0f64; N + 1]; N];
    for (row, y) in rows.iter().zip(y) {
        for i in 0..N {
            for j in 0..N {
                system[i][j] += row[i] * row[j];
            }
            system[i][N] += row[i] * y;
        }
    }
    for pivot in 0..N {
        let largest = (pivot..N)
            .max_by(|a, b| system[*a][pivot].abs().total_cmp(&system[*b][pivot].abs()))
            .unwrap();
        system.swap(pivot, largest);
        for other in 0..N {
            if other != pivot {
                let factor = system[other][pivot] / system[pivot][pivot];
                for column in pivot..=N {
                    system[other][column] -= factor * system[pivot][column];
                }
            }
        }
    }
    std::array::from_fn(|i| system[i][N] / system[i][i])
}

/// EXPERIMENT: what parrot's run-time resolution of `call imm` costs on this traffic. Runs the
/// parrot modes twice, with the lookups as they are and with them answered from a cache (see
/// `solana_sbpf::codegen::call_dispatch`), next to the legacy modes.
fn call_dispatch_experiment(blocks_directory: &Path, accounts_file: &Path, measured_rounds: usize) {
    use solana_sbpf::codegen::call_dispatch::{call_dispatch_counts, set_call_dispatch_cache};
    const CONFIGURATIONS: [(&str, ExecutionMode, bool); 6] = [
        ("legacy interp", ExecutionMode::Interpreted, false),
        ("parrot interp", ExecutionMode::ParrotInterpreted, false),
        ("parrot interp, cached call dispatch", ExecutionMode::ParrotInterpreted, true),
        ("legacy jit", ExecutionMode::Jit, false),
        ("parrot jit", ExecutionMode::ParrotJit, false),
        ("parrot jit, cached call dispatch", ExecutionMode::ParrotJit, true),
    ];
    /// The row the others are compared to.
    const LEGACY_JIT: usize = 3;

    let blocks = blocks::read_blocks(blocks_directory);
    let accounts = state::read_accounts(accounts_file);
    let (mut seashell, report) = state::seashell_from_accounts(&accounts);
    let start = state::Start::of(&seashell);
    let overhead = TimerOverhead::measure();
    println!("{} programs loaded, by SBPF version: {:?}", report.programs_loaded, report.programs_by_version);

    let mut reference: Option<Vec<Outcome>> = None;
    let mut differing = [0usize; CONFIGURATIONS.len()];
    // Milliseconds per measured round: (instructions, syscalls).
    let mut round_ms: [Vec<(f64, f64)>; CONFIGURATIONS.len()] = Default::default();
    // Instruction time per transaction, per measured round.
    let mut per_transaction_ns: [Vec<Vec<u64>>; CONFIGURATIONS.len()] = Default::default();
    // What parrot dispatched at run time, per transaction.
    let mut dispatched: Option<Vec<CallDispatchCounts>> = None;
    for round in 0..=measured_rounds {
        for (index, (_, mode, cached)) in CONFIGURATIONS.into_iter().enumerate() {
            seashell.execution_mode = mode;
            set_call_dispatch_cache(cached);
            run_blocks(&mut seashell, &start, &blocks, overhead);
            let counts_before = call_dispatch_counts();
            let run = run_blocks(&mut seashell, &start, &blocks, overhead);
            if round == 0 && index == 1 {
                let counts = call_dispatch_counts().since(&counts_before);
                println!(
                    "one pass in parrot interp resolves at run time: {} calls to the program's \
                     own functions and {} syscalls in v0 to v2 programs, {} syscalls in v3 programs",
                    counts.internal_calls, counts.syscalls, counts.static_syscalls
                );
            }
            let reference = reference.get_or_insert_with(|| run.outcomes.clone());
            differing[index] = differing[index]
                .max(run.outcomes.iter().zip(reference.iter()).filter(|(a, b)| a != b).count());
            if round > 0 {
                round_ms[index].push((
                    run.instruction_ns.iter().sum::<u64>() as f64 / 1e6,
                    run.syscall_ns.iter().sum::<u64>() as f64 / 1e6,
                ));
                per_transaction_ns[index].push(run.instruction_ns);
            }
            if index == 1 {
                dispatched = Some(run.dispatched);
            }
        }
        eprintln!("round {round} of {measured_rounds} done");
    }
    set_call_dispatch_cache(false);

    let median_and_mean = |values: Vec<f64>| {
        let mut sorted = values.clone();
        sorted.sort_by(f64::total_cmp);
        (sorted[sorted.len() / 2], values.iter().sum::<f64>() / values.len() as f64)
    };
    let total = |index: usize| {
        median_and_mean(round_ms[index].iter().map(|(instructions, syscalls)| instructions + syscalls).collect())
    };
    let instructions =
        |index: usize| median_and_mean(round_ms[index].iter().map(|(instructions, _)| *instructions).collect());
    println!();
    let reference = reference.expect("at least the warmup round ran");
    let reference_units: Vec<u64> =
        reference.iter().map(|outcome| outcome.compute_units_consumed).collect();
    println!("{} transactions, {measured_rounds} rounds, milliseconds", reference.len());
    println!();
    println!(
        "| mode | total p50 | total avg | total vs legacy jit | instructions p50 | instructions avg \
         | instructions vs legacy jit | txs differing from legacy interp |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    for (index, (name, _, _)) in CONFIGURATIONS.into_iter().enumerate() {
        let (total_p50, total_avg) = total(index);
        let (instructions_p50, instructions_avg) = instructions(index);
        println!(
            "| {name} | {total_p50:.1} | {total_avg:.1} | {:.2}x | {instructions_p50:.1} | \
             {instructions_avg:.1} | {:.2}x | {} |",
            total(LEGACY_JIT).0 / total_p50,
            instructions(LEGACY_JIT).0 / instructions_p50,
            differing[index],
        );
    }

    // Where parrot jit loses to legacy jit, transaction by transaction.
    const PARROT_JIT: usize = 4;
    let dispatched = dispatched.expect("parrot interp ran");
    let internal_calls: Vec<u64> = dispatched.iter().map(|counts| counts.internal_calls).collect();
    let syscalls: Vec<u64> =
        dispatched.iter().map(|counts| counts.syscalls + counts.static_syscalls).collect();
    let median_ns = |configuration: usize, transaction: usize| {
        let mut samples: Vec<u64> =
            per_transaction_ns[configuration].iter().map(|round| round[transaction]).collect();
        samples.sort_unstable();
        samples[samples.len() / 2] as f64
    };
    let transactions = internal_calls.len();
    let legacy: Vec<f64> = (0..transactions).map(|tx| median_ns(LEGACY_JIT, tx)).collect();
    let parrot: Vec<f64> = (0..transactions).map(|tx| median_ns(PARROT_JIT, tx)).collect();

    println!();
    println!("parrot jit against legacy jit, instruction time, by how many calls to its own functions a transaction makes");
    println!();
    println!(
        "| own-function calls in the tx | txs | own-function calls | syscalls | legacy jit (ms) | \
         parrot jit (ms) | gap (ms) | gap per dispatched call (ns) |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    let bins: [(&str, u64, u64); 6] = [
        ("0", 0, 0),
        ("1 to 10", 1, 10),
        ("11 to 100", 11, 100),
        ("101 to 1,000", 101, 1_000),
        ("1,001 to 10,000", 1_001, 10_000),
        ("over 10,000", 10_001, u64::MAX),
    ];
    for (label, low, high) in bins {
        let members: Vec<usize> =
            (0..transactions).filter(|&tx| (low..=high).contains(&internal_calls[tx])).collect();
        let calls: u64 = members.iter().map(|&tx| internal_calls[tx]).sum();
        let bin_syscalls: u64 = members.iter().map(|&tx| syscalls[tx]).sum();
        let legacy_ms = members.iter().map(|&tx| legacy[tx]).sum::<f64>() / 1e6;
        let parrot_ms = members.iter().map(|&tx| parrot[tx]).sum::<f64>() / 1e6;
        let dispatched_calls = (calls + bin_syscalls).max(1) as f64;
        println!(
            "| {label} | {} | {calls} | {bin_syscalls} | {legacy_ms:.1} | {parrot_ms:.1} | {:.1} | \
             {:.1} |",
            members.len(),
            parrot_ms - legacy_ms,
            (parrot_ms - legacy_ms) * 1e6 / dispatched_calls,
        );
    }

    // gap of a transaction = a * own-function calls + b * syscalls + c * compute units
    let features: Vec<[f64; 3]> = (0..transactions)
        .map(|tx| {
            [
                internal_calls[tx] as f64,
                syscalls[tx] as f64,
                reference_units[tx] as f64,
            ]
        })
        .collect();
    let gaps: Vec<f64> = (0..transactions).map(|tx| parrot[tx] - legacy[tx]).collect();
    let [per_call, per_syscall, per_unit] = least_squares(&features, &gaps);
    let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
    let (mut residual, mut variation) = (0.0, 0.0);
    for (row, gap) in features.iter().zip(&gaps) {
        let predicted = row[0] * per_call + row[1] * per_syscall + row[2] * per_unit;
        residual += (gap - predicted).powi(2);
        variation += (gap - mean).powi(2);
    }
    let sum = |column: usize| features.iter().map(|row| row[column]).sum::<f64>();
    println!();
    println!(
        "least squares, gap per transaction = a * own-function calls + b * syscalls + c * compute \
         units (R² {:.3})",
        1.0 - residual / variation
    );
    println!();
    println!("| term | count | ns each | gap it accounts for (ms) |");
    println!("|---|---:|---:|---:|");
    for (name, column, each) in [
        ("own-function calls", 0, per_call),
        ("syscalls", 1, per_syscall),
        ("compute units", 2, per_unit),
    ] {
        println!("| {name} | {:.0} | {each:.2} | {:.1} |", sum(column), sum(column) * each / 1e6);
    }
    println!("| (measured gap) | | | {:.1} |", gaps.iter().sum::<f64>() / 1e6);
}

/// Time per syscall and mode, as the median over the rounds.
fn print_syscalls(by_syscall: &[Vec<HashMap<&'static str, (u64, u64)>>; MODES.len()]) {
    let median_ms = |mode: usize, name: &str| {
        let mut samples: Vec<u64> = by_syscall[mode]
            .iter()
            .map(|round| round.get(name).map_or(0, |(_, ns)| *ns))
            .collect();
        samples.sort_unstable();
        samples[samples.len() / 2] as f64 / 1e6
    };
    let mut names: Vec<(&'static str, u64)> =
        by_syscall[0][0].iter().map(|(name, (calls, _))| (*name, *calls)).collect();
    names.sort_by(|a, b| median_ms(0, b.0).total_cmp(&median_ms(0, a.0)));

    println!();
    println!("time by syscall (ms, median over the rounds), without the program runs a CPI starts");
    println!();
    println!(
        "| syscall | calls | ns per call (legacy jit) | legacy interp | legacy jit | parrot interp \
         | parrot jit |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|");
    for (name, calls) in names {
        let ms: [f64; MODES.len()] = std::array::from_fn(|mode| median_ms(mode, name));
        // `solana_syscalls::SyscallInvokeSignedRust` and the like.
        let short = name.rsplit("::").next().unwrap_or(name);
        println!(
            "| {short} | {calls} | {:.0} | {:.1} | {:.1} | {:.1} | {:.1} |",
            ms[1] * 1e6 / calls as f64,
            ms[0],
            ms[1],
            ms[2],
            ms[3],
        );
    }
}

/// Program instruction time by the program a transaction is for: its first top-level
/// instruction that is not a compute budget instruction. A transaction's time is its median
/// over the rounds, and includes the programs it calls into.
fn print_programs(
    cluster: &[&blocks::BlockTransaction],
    execution_ns: &[Vec<Vec<u64>>; MODES.len()],
) {
    const ROWS: usize = 15;
    let median_ns = |mode: usize, transaction: usize| {
        let mut samples: Vec<u64> =
            execution_ns[mode].iter().map(|round| round[transaction]).collect();
        samples.sort_unstable();
        samples[samples.len() / 2]
    };
    // program → (transactions, time per mode)
    let mut programs: HashMap<Pubkey, (usize, [u64; MODES.len()])> = HashMap::new();
    for (index, transaction) in cluster.iter().enumerate() {
        let message = transaction.transaction.message();
        let Some(program) = message
            .program_instructions_iter()
            .map(|(program, _)| *program)
            .find(|program| *program != solana_sdk_ids::compute_budget::id())
        else {
            continue;
        };
        let entry = programs.entry(program).or_default();
        entry.0 += 1;
        for mode in 0..MODES.len() {
            entry.1[mode] += median_ns(mode, index);
        }
    }
    let mut programs: Vec<_> = programs.into_iter().collect();
    programs.sort_by_key(|(_, (_, ns))| std::cmp::Reverse(ns[0]));

    println!();
    println!("program instructions by program, top {ROWS} by legacy interp time (ms)");
    println!();
    println!(
        "| program | txs | legacy interp | legacy jit | parrot interp | parrot jit | parrot \
         interp vs legacy interp | parrot jit vs legacy jit |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    for (program, (transactions, ns)) in programs.iter().take(ROWS) {
        let ms = ns.map(|ns| ns as f64 / 1e6);
        println!(
            "| {program} | {transactions} | {:.1} | {:.1} | {:.1} | {:.1} | {:.2}x | {:.2}x |",
            ms[0],
            ms[1],
            ms[2],
            ms[3],
            ms[0] / ms[2],
            ms[1] / ms[3],
        );
    }
}

/// One row per mode: the program execution time of the selected transactions, summed over a
/// round, as the median and the mean over the rounds.
fn print_table(
    title: &str,
    execution_ns: &[Vec<Vec<u64>>; MODES.len()],
    selected: &[bool],
    measured_rounds: usize,
) {
    println!();
    println!("{title}, {measured_rounds} rounds");
    println!();
    println!("| mode | p50 exec (ms) | avg exec (ms) | vs legacy interp |");
    println!("|---|---:|---:|---:|");
    let mut legacy_interp_p50 = 0.0;
    for (index, (name, _)) in MODES.into_iter().enumerate() {
        let mut round_ms: Vec<f64> = execution_ns[index]
            .iter()
            .map(|round| {
                let nanoseconds: u64 = round
                    .iter()
                    .zip(selected)
                    .filter(|(_, &selected)| selected)
                    .map(|(nanoseconds, _)| nanoseconds)
                    .sum();
                nanoseconds as f64 / 1e6
            })
            .collect();
        round_ms.sort_by(f64::total_cmp);
        let p50 = round_ms[round_ms.len() / 2];
        let avg = round_ms.iter().sum::<f64>() / round_ms.len() as f64;
        if index == 0 {
            legacy_interp_p50 = p50;
        }
        println!("| {name} | {p50:.1} | {avg:.1} | {:.2}x |", legacy_interp_p50 / p50);
    }
}
