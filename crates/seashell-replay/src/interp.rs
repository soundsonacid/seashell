//! EXPERIMENT: does parrot's interpreter lose to the legacy one on the programs whose hot code
//! does not fit the L1 instruction cache the way parrot lays its steps out?
//!
//! Parrot's interpreter has one 128-byte step per (opcode, destination register, source
//! register), at `base + (op | dst << 8 | src << 12) * 128`. An L1 instruction cache of 64 sets
//! and 8 ways picks the set from address bits 6 to 11, which here are the low five bits of the
//! opcode. So all the steps that share those five bits compete for 8 places.
//!
//! For each program this measures, from the instructions it actually executes:
//!
//! - **overflow**: the share of executed instructions whose step is not among the 8 most
//!   executed steps of its cache set. A program with no overflow can keep every step it uses
//!   hot. This is a lower bound on what does not fit, since it ignores everything else that
//!   shares the cache.
//! - the instruction time of the program under each interpreter.

use std::collections::HashMap;

use seashell::{ExecutionMode, Seashell};
use solana_program_runtime::program_cache::entry::ProgramCacheEntryType;
use solana_pubkey::Pubkey;

use crate::blocks::Block;
use crate::state::Start;
use crate::{run_blocks, TimerOverhead};

/// The steps that share a cache set: the low five bits of the opcode.
const SETS: usize = 32;
const WAYS: usize = 8;

struct Program {
    address: Pubkey,
    version: String,
    runs: u64,
    instructions: u64,
    /// Share of executed instructions outside the `WAYS` most executed steps of their set.
    overflow: f64,
    /// Distinct steps executed.
    steps: usize,
    legacy_ms: f64,
    parrot_ms: f64,
}

/// Overflow share and distinct steps of a step profile.
fn overflow(counts: &[u64]) -> (f64, usize) {
    let mut by_set: Vec<Vec<u64>> = vec![Vec::new(); SETS];
    for (step, &count) in counts.iter().enumerate() {
        if count > 0 {
            by_set[step & (SETS - 1)].push(count);
        }
    }
    let total: u64 = counts.iter().sum();
    let mut outside = 0;
    for set in &mut by_set {
        set.sort_unstable_by(|a, b| b.cmp(a));
        outside += set.iter().skip(WAYS).sum::<u64>();
    }
    let steps = by_set.iter().map(Vec::len).sum();
    (outside as f64 / total.max(1) as f64, steps)
}

pub fn report(seashell: &mut Seashell, start: &Start, blocks: &[Block], measured_rounds: usize) {
    use solana_sbpf::step_profile::{set_step_profile, take_step_profiles};
    use solana_sbpf::timing::executable_times;
    const INTERPRETERS: [ExecutionMode; 2] =
        [ExecutionMode::Interpreted, ExecutionMode::ParrotInterpreted];
    let overhead = TimerOverhead::measure();

    // Which executable is which program.
    let mut programs: HashMap<usize, (Pubkey, String)> = HashMap::new();
    for address in start.accounts.keys() {
        let Some(entry) = seashell.accounts_db.programs.get_entry_for_tests(address) else {
            continue;
        };
        if let ProgramCacheEntryType::Loaded(executable) = &entry.program {
            programs.insert(
                std::ptr::from_ref(executable) as usize,
                (*address, format!("{:?}", executable.get_sbpf_version())),
            );
        }
    }

    // What each program executes.
    seashell.execution_mode = ExecutionMode::Interpreted;
    set_step_profile(true);
    run_blocks(seashell, start, blocks, overhead);
    set_step_profile(false);
    let profiles = take_step_profiles();

    // Instruction time per program and interpreter, in milliseconds per measured round.
    let mut round_ms: [HashMap<usize, Vec<f64>>; 2] = Default::default();
    let mut runs: HashMap<usize, u64> = HashMap::new();
    for round in 0..=measured_rounds {
        for (index, mode) in INTERPRETERS.into_iter().enumerate() {
            seashell.execution_mode = mode;
            run_blocks(seashell, start, blocks, overhead);
            let before = executable_times();
            run_blocks(seashell, start, blocks, overhead);
            if round == 0 {
                continue;
            }
            for (executable, after) in executable_times() {
                let before = before.get(&executable).copied().unwrap_or_default();
                let ns = TimerOverhead::without(
                    after.instructions_ns - before.instructions_ns,
                    after.syscalls - before.syscalls,
                    overhead.in_instructions,
                );
                round_ms[index].entry(executable).or_default().push(ns as f64 / 1e6);
                runs.insert(executable, after.program_runs - before.program_runs);
            }
        }
        eprintln!("round {round} of {measured_rounds} done");
    }
    let median = |samples: &[f64]| {
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[sorted.len() / 2]
    };

    let mut table: Vec<Program> = Vec::new();
    for (executable, counts) in &profiles {
        let (Some((address, version)), Some(legacy), Some(parrot)) = (
            programs.get(executable),
            round_ms[0].get(executable),
            round_ms[1].get(executable),
        ) else {
            continue;
        };
        let (overflow, steps) = overflow(counts);
        table.push(Program {
            address: *address,
            version: version.clone(),
            runs: runs.get(executable).copied().unwrap_or(0),
            instructions: counts.iter().sum(),
            overflow,
            steps,
            legacy_ms: median(legacy),
            parrot_ms: median(parrot),
        });
    }
    table.sort_by(|a, b| b.legacy_ms.total_cmp(&a.legacy_ms));

    let total = |field: &dyn Fn(&Program) -> f64, members: &[&Program]| -> f64 {
        members.iter().map(|program| field(program)).sum()
    };
    println!(
        "{} programs executed, {measured_rounds} rounds. instruction time is each program's own \
         (not its callees'), median over the rounds",
        table.len()
    );

    println!();
    println!("programs grouped by overflow: the share of executed instructions outside the {WAYS} most executed steps of their L1i set");
    println!();
    println!(
        "| overflow | programs | instructions executed | legacy interp (ms) | parrot interp (ms) \
         | parrot vs legacy | parrot minus legacy (ms) |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|");
    let bins: [(&str, f64, f64); 6] = [
        ("under 5%", 0.0, 0.05),
        ("5% to 15%", 0.05, 0.15),
        ("15% to 30%", 0.15, 0.30),
        ("30% to 50%", 0.30, 0.50),
        ("50% and over", 0.50, f64::INFINITY),
        ("all", 0.0, f64::INFINITY),
    ];
    for (label, low, high) in bins {
        let members: Vec<&Program> =
            table.iter().filter(|program| program.overflow >= low && program.overflow < high).collect();
        let legacy = total(&|program| program.legacy_ms, &members);
        let parrot = total(&|program| program.parrot_ms, &members);
        println!(
            "| {label} | {} | {} | {legacy:.1} | {parrot:.1} | {:.2}x | {:+.1} |",
            members.len(),
            members.iter().map(|program| program.instructions).sum::<u64>(),
            legacy / parrot,
            parrot - legacy,
        );
    }

    let row = |program: &Program| {
        println!(
            "| {} | {} | {} | {:.1} | {} | {:.0}% | {:.1} | {:.1} | {:.2}x |",
            program.address,
            program.version,
            program.runs,
            program.instructions as f64 / 1e6,
            program.steps,
            100.0 * program.overflow,
            program.legacy_ms,
            program.parrot_ms,
            program.legacy_ms / program.parrot_ms,
        )
    };
    let header = || {
        println!();
        println!(
            "| program | version | runs | instructions (M) | distinct steps | overflow | legacy \
             interp (ms) | parrot interp (ms) | parrot vs legacy |"
        );
        println!("|---|---|---:|---:|---:|---:|---:|---:|---:|");
    };

    println!();
    println!("the 30 programs with the most legacy interp time");
    header();
    table.iter().take(30).for_each(row);

    // Programs with enough time for the ratio to mean something.
    const MINIMUM_MS: f64 = 5.0;
    let mut sizable: Vec<&Program> =
        table.iter().filter(|program| program.legacy_ms >= MINIMUM_MS).collect();
    sizable.sort_by(|a, b| {
        (a.legacy_ms / a.parrot_ms).total_cmp(&(b.legacy_ms / b.parrot_ms))
    });
    println!();
    println!("of the {} programs with at least {MINIMUM_MS} ms, the 15 where parrot interp does worst", sizable.len());
    header();
    sizable.iter().take(15).for_each(|program| row(program));
    println!();
    println!("and the 15 where it does best");
    header();
    sizable.iter().rev().take(15).for_each(|program| row(program));

    // How well overflow predicts the ratio, weighting programs by their legacy time.
    let weight: f64 = sizable.iter().map(|program| program.legacy_ms).sum();
    let mean = |value: &dyn Fn(&Program) -> f64| {
        sizable.iter().map(|program| program.legacy_ms * value(program)).sum::<f64>() / weight
    };
    let ratio = |program: &Program| (program.parrot_ms / program.legacy_ms).ln();
    let (mean_overflow, mean_ratio) = (mean(&|program| program.overflow), mean(&ratio));
    let covariance = mean(&|program| (program.overflow - mean_overflow) * (ratio(program) - mean_ratio));
    let variance_overflow = mean(&|program| (program.overflow - mean_overflow).powi(2));
    let variance_ratio = mean(&|program| (ratio(program) - mean_ratio).powi(2));
    println!();
    println!(
        "correlation between overflow and log(parrot time / legacy time) over those {} programs, \
         weighted by legacy time: {:.2}",
        sizable.len(),
        covariance / (variance_overflow * variance_ratio).sqrt()
    );
}
