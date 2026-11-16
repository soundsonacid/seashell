//! How the replay differs from what the cluster recorded, per execution mode.
//!
//! A transaction matches when it ends the same way (success or failure) with the same compute
//! units. For one that does not, the program logs of the two say where they part: both runtimes
//! log every program invocation, what it consumed and how it ended, so the first of those lines
//! that differs names the program and the kind of difference.

use std::collections::HashMap;

use seashell::{ExecutionMode, Seashell};
use solana_account::{ReadableAccount, WritableAccount};
use solana_svm_log_collector::LogCollector;

use crate::blocks::{Block, BlockTransaction};
use crate::state::Start;
use crate::MODES;

/// What the replay recorded for one transaction.
#[derive(Clone, PartialEq, Eq)]
struct Replayed {
    succeeded: bool,
    compute_units_consumed: u64,
    logs: Vec<String>,
}

/// One of the log lines the runtime writes about a program invocation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Event<'a> {
    Invoke { program: &'a str },
    Consumed { program: &'a str, units: u64, remaining: u64 },
    Success { program: &'a str },
    Failed { program: &'a str, why: &'a str },
}

impl<'a> Event<'a> {
    fn program(&self) -> &'a str {
        match self {
            Event::Invoke { program }
            | Event::Consumed { program, .. }
            | Event::Success { program }
            | Event::Failed { program, .. } => program,
        }
    }
}

/// The invocation event `line` is, if it is one. Lines that programs write themselves
/// (`Program log:`, `Program data:`, `Program return:`) are not.
fn event(line: &str) -> Option<Event<'_>> {
    let rest = line.strip_prefix("Program ")?;
    let (program, rest) = rest.split_once(' ')?;
    if program.ends_with(':') {
        return None;
    }
    if rest.starts_with("invoke [") {
        Some(Event::Invoke { program })
    } else if rest == "success" {
        Some(Event::Success { program })
    } else if let Some(why) = rest.strip_prefix("failed: ") {
        Some(Event::Failed { program, why })
    } else if let Some(rest) = rest.strip_prefix("consumed ") {
        let (units, rest) = rest.split_once(" of ")?;
        let (remaining, _) = rest.split_once(' ')?;
        Some(Event::Consumed { program, units: units.parse().ok()?, remaining: remaining.parse().ok()? })
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub(crate) enum Kind {
    /// A program consumed a different number of compute units.
    ComputeUnits,
    /// A program started with a different number of compute units left, having consumed the
    /// same before.
    Budget,
    /// A program succeeded on the cluster and fails in the replay.
    ReplayFails,
    /// A program failed on the cluster and succeeds in the replay.
    ClusterFails,
    /// A program fails in both, for different reasons.
    DifferentError,
    /// A program makes a different call, or one log simply continues where the other stops.
    DifferentPath,
    /// The logs agree as far as the cluster's go.
    NotInLogs,
}

const KINDS: [(Kind, &str); 7] = [
    (Kind::ComputeUnits, "CU differs"),
    (Kind::Budget, "CU left differs"),
    (Kind::ReplayFails, "replay fails"),
    (Kind::ClusterFails, "cluster fails"),
    (Kind::DifferentError, "other error"),
    (Kind::DifferentPath, "other path"),
    (Kind::NotInLogs, "not in logs"),
];

pub(crate) struct Divergence {
    pub(crate) kind: Kind,
    /// The program the two part in. For a different call, the caller.
    pub(crate) program: String,
    /// Replay minus cluster, for `Kind::ComputeUnits`.
    pub(crate) units_delta: i64,
    pub(crate) cluster_line: String,
    pub(crate) replay_line: String,
}

/// Where the logs of a transaction first differ.
pub(crate) fn divergence(cluster_logs: &[String], replay_logs: &[String]) -> Divergence {
    let cluster_truncated = cluster_logs.iter().any(|line| line == "Log truncated");
    let mut cluster = cluster_logs.iter().filter_map(|line| event(line).map(|event| (event, line)));
    let mut replay = replay_logs.iter().filter_map(|line| event(line).map(|event| (event, line)));
    // The programs that are running, innermost last.
    let mut stack: Vec<&str> = Vec::new();
    let top_or = |stack: &[&str], fallback: &str| stack.last().copied().unwrap_or(fallback).to_string();

    loop {
        let (kind, program, units_delta, cluster_line, replay_line) =
            match (cluster.next(), replay.next()) {
                (None, None) => {
                    return Divergence {
                        kind: Kind::NotInLogs,
                        program: "(none)".to_string(),
                        units_delta: 0,
                        cluster_line: String::new(),
                        replay_line: String::new(),
                    }
                }
                (Some((theirs, _)), Some((ours, _))) if theirs == ours => {
                    match theirs {
                        Event::Invoke { program } => stack.push(program),
                        Event::Success { .. } | Event::Failed { .. } => {
                            stack.pop();
                        }
                        Event::Consumed { .. } => {}
                    }
                    continue;
                }
                (Some((theirs, their_line)), Some((ours, our_line))) => {
                    let (kind, units_delta) = match (theirs, ours) {
                        (Event::Consumed { units: a, .. }, Event::Consumed { units: b, .. })
                            if a != b =>
                        {
                            (Kind::ComputeUnits, b as i64 - a as i64)
                        }
                        (Event::Consumed { .. }, Event::Consumed { .. }) => (Kind::Budget, 0),
                        (Event::Success { .. }, Event::Failed { .. }) => (Kind::ReplayFails, 0),
                        (Event::Failed { .. }, Event::Success { .. }) => (Kind::ClusterFails, 0),
                        (Event::Failed { .. }, Event::Failed { .. }) => (Kind::DifferentError, 0),
                        _ => (Kind::DifferentPath, 0),
                    };
                    let program = match (theirs, ours) {
                        (Event::Invoke { .. }, _) | (_, Event::Invoke { .. }) => {
                            top_or(&stack, theirs.program())
                        }
                        // The program consumed the same, so the units went missing before it
                        // ran: in its caller, or in earlier instructions of the transaction.
                        _ if kind == Kind::Budget => match stack.len().checked_sub(2) {
                            Some(caller) => format!("{} (as caller)", stack[caller]),
                            None => "(before the instruction, at the top level)".to_string(),
                        },
                        _ => theirs.program().to_string(),
                    };
                    (kind, program, units_delta, their_line.clone(), our_line.clone())
                }
                // The cluster keeps only the first 10 kB of a transaction's logs.
                (None, Some(_)) if cluster_truncated => {
                    (Kind::NotInLogs, "(cluster log truncated)".to_string(), 0, String::new(), String::new())
                }
                (None, Some((ours, our_line))) => {
                    (Kind::DifferentPath, top_or(&stack, ours.program()), 0, String::new(), our_line.clone())
                }
                (Some((theirs, their_line)), None) => {
                    (Kind::DifferentPath, top_or(&stack, theirs.program()), 0, their_line.clone(), String::new())
                }
            };
        return Divergence { kind, program, units_delta, cluster_line, replay_line };
    }
}

/// Runs every transaction once in `mode`, keeping the logs.
fn replay(
    seashell: &mut Seashell,
    start: &Start,
    blocks: &[Block],
    mode: ExecutionMode,
) -> Vec<Replayed> {
    start.restore_accounts(seashell);
    seashell.execution_mode = mode;
    let mut replayed = Vec::new();
    for (block_index, _) in blocks.iter().enumerate() {
        crate::blocks::enter_block(seashell, blocks, block_index, start);
        let block = &blocks[block_index];
        for transaction in &block.transactions {
            let fee_payer = *transaction.transaction.message().fee_payer();
            let mut account = seashell.accounts_db.account_maybe(&fee_payer).unwrap_or_default();
            account.set_lamports(account.lamports().saturating_sub(transaction.fee));
            seashell.accounts_db.set_account(fee_payer, account);

            let log_collector = LogCollector::new_ref_with_limit(None);
            seashell.log_collector = Some(log_collector.clone());
            let limits = crate::blocks::limits(seashell, &transaction.transaction);
            let result = seashell.process_transaction(&transaction.transaction, limits);
            seashell.log_collector = None;
            replayed.push(Replayed {
                succeeded: result.error.is_none(),
                compute_units_consumed: result.compute_units_consumed,
                logs: log_collector.borrow().get_recorded_content().to_vec(),
            });
        }
    }
    replayed
}

/// Upper bounds of the compute unit difference buckets, and of the relative ones in percent.
const UNIT_BUCKETS: [u64; 6] = [10, 100, 1_000, 10_000, 100_000, u64::MAX];
const PERCENT_BUCKETS: [f64; 6] = [0.1, 1.0, 5.0, 20.0, 50.0, f64::INFINITY];

fn bucket_label(bounds: &[String], index: usize) -> String {
    match index {
        0 => format!("up to {}", bounds[0]),
        _ if index + 1 == bounds.len() => format!("over {}", bounds[index - 1]),
        _ => format!("{} to {}", bounds[index - 1], bounds[index]),
    }
}

pub fn report(seashell: &mut Seashell, start: &Start, blocks: &[Block]) {
    let cluster: Vec<(u64, &BlockTransaction)> = blocks
        .iter()
        .flat_map(|block| block.transactions.iter().map(|transaction| (block.slot, transaction)))
        .collect();
    let replays: Vec<Vec<Replayed>> = MODES
        .iter()
        .map(|(name, mode)| {
            eprintln!("replaying in {name}");
            replay(seashell, start, blocks, *mode)
        })
        .collect();
    let mode_header: String = MODES.iter().map(|(name, _)| format!(" {name} |")).collect();
    let mode_rule: String = MODES.iter().map(|_| "---:|").collect();

    println!();
    println!("the modes against each other (outcome, compute units and every log line)");
    println!();
    println!("| mode | transactions that differ from legacy interp |");
    println!("|---|---:|");
    for (index, (name, _)) in MODES.iter().enumerate() {
        let differing =
            replays[index].iter().zip(&replays[0]).filter(|(ours, theirs)| ours != theirs).count();
        println!("| {name} | {differing} |");
    }

    // One row per quantity, one column per mode.
    let rows = |title: &str, labels: &[String], count: &dyn Fn(&Replayed, &BlockTransaction) -> Option<usize>| {
        println!();
        println!("{title}");
        println!();
        println!("| |{mode_header}");
        println!("|---|{mode_rule}");
        let mut counts = vec![[0usize; MODES.len()]; labels.len()];
        for (mode, replayed) in replays.iter().enumerate() {
            for (ours, (_, theirs)) in replayed.iter().zip(&cluster) {
                if let Some(row) = count(ours, theirs) {
                    counts[row][mode] += 1;
                }
            }
        }
        for (label, counts) in labels.iter().zip(&counts) {
            let cells: String = counts.iter().map(|count| format!(" {count} |")).collect();
            println!("| {label} |{cells}");
        }
    };
    let labels = |labels: &[&str]| labels.iter().map(|label| label.to_string()).collect::<Vec<_>>();

    rows(
        &format!("how transactions end, replay against the cluster ({} transactions)", cluster.len()),
        &labels(&[
            "succeeds on both",
            "fails on both",
            "succeeds on the cluster, fails in the replay",
            "fails on the cluster, succeeds in the replay",
        ]),
        &|ours, theirs| {
            Some(match (theirs.succeeded, ours.succeeded) {
                (true, true) => 0,
                (false, false) => 1,
                (true, false) => 2,
                (false, true) => 3,
            })
        },
    );
    rows(
        "compute units, by how the transaction ends",
        &labels(&[
            "succeeds on both, same CU",
            "succeeds on both, different CU",
            "fails on both, same CU",
            "fails on both, different CU",
        ]),
        &|ours, theirs| {
            let same = theirs.compute_units_consumed == Some(ours.compute_units_consumed);
            match (theirs.succeeded, ours.succeeded) {
                (true, true) => Some(if same { 0 } else { 1 }),
                (false, false) => Some(if same { 2 } else { 3 }),
                _ => None,
            }
        },
    );

    // Distribution of the compute unit difference, for the transactions that end the same way.
    let unit_bounds: Vec<String> = UNIT_BUCKETS.iter().map(|bound| bound.to_string()).collect();
    let percent_bounds: Vec<String> =
        PERCENT_BUCKETS.iter().map(|bound| format!("{bound}%")).collect();
    let signed_labels = |bounds: &[String], unit: &str| -> Vec<String> {
        let lower = (0..bounds.len())
            .rev()
            .map(|index| format!("replay lower by {}{unit}", bucket_label(bounds, index)));
        let higher = (0..bounds.len())
            .map(|index| format!("replay higher by {}{unit}", bucket_label(bounds, index)));
        lower.chain(std::iter::once("equal".to_string())).chain(higher).collect()
    };
    // Row of a signed difference whose magnitude falls in bucket `bucket` of `buckets` buckets.
    let signed_row = |buckets: usize, bucket: usize, replay_higher: bool| {
        if replay_higher {
            buckets + 1 + bucket
        } else {
            buckets - 1 - bucket
        }
    };
    let same_ending_units = |ours: &Replayed, theirs: &BlockTransaction| -> Option<(u64, u64)> {
        (theirs.succeeded == ours.succeeded)
            .then_some(theirs.compute_units_consumed)
            .flatten()
            .map(|cluster_units| (cluster_units, ours.compute_units_consumed))
    };
    rows(
        "compute unit difference (replay minus cluster), transactions that end the same way",
        &signed_labels(&unit_bounds, " CU"),
        &|ours, theirs| {
            let (cluster_units, replay_units) = same_ending_units(ours, theirs)?;
            if cluster_units == replay_units {
                return Some(UNIT_BUCKETS.len());
            }
            let difference = cluster_units.abs_diff(replay_units);
            let bucket = UNIT_BUCKETS.iter().position(|bound| difference <= *bound).unwrap();
            Some(signed_row(UNIT_BUCKETS.len(), bucket, replay_units > cluster_units))
        },
    );
    rows(
        "the same difference as a share of the cluster's compute units",
        &signed_labels(&percent_bounds, ""),
        &|ours, theirs| {
            let (cluster_units, replay_units) = same_ending_units(ours, theirs)?;
            if cluster_units == replay_units {
                return Some(PERCENT_BUCKETS.len());
            }
            let percent =
                100.0 * cluster_units.abs_diff(replay_units) as f64 / cluster_units.max(1) as f64;
            let bucket = PERCENT_BUCKETS.iter().position(|bound| percent <= *bound).unwrap();
            Some(signed_row(PERCENT_BUCKETS.len(), bucket, replay_units > cluster_units))
        },
    );

    // Match rate over time.
    println!();
    println!("exact matches (same ending, same CU) by slot, legacy interp");
    println!();
    println!("| slots | transactions | exact matches | share |");
    println!("|---|---:|---:|---:|");
    let matches = |ours: &Replayed, theirs: &BlockTransaction| {
        ours.succeeded == theirs.succeeded
            && theirs.compute_units_consumed == Some(ours.compute_units_consumed)
    };
    for group in blocks.chunks(blocks.len().div_ceil(10).max(1)) {
        let (first, last) = (group[0].slot, group[group.len() - 1].slot);
        let in_group: Vec<bool> = replays[0]
            .iter()
            .zip(&cluster)
            .filter(|(_, (slot, _))| (first..=last).contains(slot))
            .map(|(ours, (_, theirs))| matches(ours, theirs))
            .collect();
        let matched = in_group.iter().filter(|&&matched| matched).count();
        println!(
            "| {first}..={last} | {} | {matched} | {:.1}% |",
            in_group.len(),
            100.0 * matched as f64 / in_group.len().max(1) as f64
        );
    }

    // Where the transactions that do not match part ways, by program.
    #[derive(Default)]
    struct Program {
        invoked_in: usize,
        kinds: HashMap<Kind, usize>,
        units_deltas: Vec<i64>,
        example: Option<(String, Divergence)>,
    }
    let mut programs: HashMap<String, Program> = HashMap::new();
    let mut mismatched = 0;
    for (ours, (_, theirs)) in replays[0].iter().zip(&cluster) {
        let mut invoked: Vec<&str> = theirs
            .log_messages
            .iter()
            .filter_map(|line| match event(line) {
                Some(Event::Invoke { program }) => Some(program),
                _ => None,
            })
            .collect();
        invoked.sort_unstable();
        invoked.dedup();
        for program in invoked {
            programs.entry(program.to_string()).or_default().invoked_in += 1;
        }
        if matches(ours, theirs) {
            continue;
        }
        mismatched += 1;
        let divergence = divergence(&theirs.log_messages, &ours.logs);
        let program = programs.entry(divergence.program.clone()).or_default();
        *program.kinds.entry(divergence.kind).or_default() += 1;
        if divergence.kind == Kind::ComputeUnits {
            program.units_deltas.push(divergence.units_delta);
        }
        program.example.get_or_insert((theirs.signature.clone(), divergence));
    }
    let mut programs: Vec<(String, Program)> = programs
        .into_iter()
        .filter(|(_, program)| !program.kinds.is_empty())
        .collect();
    let total = |program: &Program| program.kinds.values().sum::<usize>();
    programs.sort_by(|a, b| total(&b.1).cmp(&total(&a.1)).then(a.0.cmp(&b.0)));

    const ROWS: usize = 30;
    println!();
    println!(
        "the {mismatched} transactions that do not match, by the program their logs first \
         differ in (legacy interp, top {ROWS})"
    );
    println!();
    let kind_header: String = KINDS.iter().map(|(_, name)| format!(" {name} |")).collect();
    println!(
        "| program | transactions invoking it | first difference here |{kind_header} median CU \
         difference |"
    );
    println!("|---|---:|---:|{}---:|", "---:|".repeat(KINDS.len()));
    for (name, program) in programs.iter().take(ROWS) {
        let kinds: String = KINDS
            .iter()
            .map(|(kind, _)| format!(" {} |", program.kinds.get(kind).copied().unwrap_or(0)))
            .collect();
        let mut deltas = program.units_deltas.clone();
        deltas.sort_unstable();
        let median = deltas.get(deltas.len() / 2).map_or(String::new(), |delta| format!("{delta:+}"));
        println!(
            "| {name} | {} | {} |{kinds} {median} |",
            program.invoked_in,
            total(program)
        );
    }
    println!(
        "| (all {} programs) | | {} | | | | | | | | |",
        programs.len(),
        programs.iter().map(|(_, program)| total(program)).sum::<usize>()
    );

    println!();
    println!("one example per program (first {ROWS})");
    for (name, program) in programs.iter().take(ROWS) {
        let Some((signature, divergence)) = &program.example else {
            continue;
        };
        println!();
        println!("{name}: {:?} in {signature}", divergence.kind);
        println!("  cluster: {}", divergence.cluster_line);
        println!("  replay:  {}", divergence.replay_line);
    }
}
