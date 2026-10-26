//! Wall-clock benchmark: `sol_math` (AVX-512 lowering on hosts that have it) against the same
//! formula as a JIT-compiled BPF loop.
//!
//! One instruction's wall time is mostly VM and transaction setup, so the program repeats the
//! kernel `repeats` times inside a single invocation and the benchmark fits
//! `time = setup + repeats * kernel` over several repeat counts. The slope is the time one
//! kernel invocation costs inside the running program; the intercept is everything else.
//! Every (kernel, implementation, lanes, repeats) point is sampled in shuffled rounds so clock
//! drift lands on all of them alike.
//!
//! ```text
//! cargo test --release -p seashell --test sol_math_bench -- --ignored --nocapture
//! ```
//! `SOL_MATH_BENCH_OUT` names a directory for the raw samples and fits (CSV).

use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_syscalls::math::{encode, A, ADD, B, C, MUL128_DIV, MUL128_SHR, MUL_HI52, SUB};
use std::fmt::Write as _;
use std::time::Instant;

const PATTERN_NOOP: u8 = 0;
const PATTERN_SOL_MATH: u8 = 1;
const PATTERN_BPF_MUL128_SHR: u8 = 2;
const PATTERN_BPF_LERP52: u8 = 3;
const PATTERN_BPF_ADD: u8 = 4;
const PATTERN_BPF_MUL_DIV: u8 = 5;
const PATTERN_BPF_ADD_U128: u8 = 6;
const PATTERN_BPF_MUL128_SHR_U128: u8 = 7;
const PATTERN_BPF_MUL_DIV_U128: u8 = 8;
const PATTERN_BPF_LERP52_U128: u8 = 9;

const FLAG_WIDE: u8 = 0b0001;
const VECTOR_A: u8 = 0b0010;
const VECTOR_B: u8 = 0b0100;
const VECTOR_C: u8 = 0b1000;
const VECTOR_ALL: u8 = VECTOR_A | VECTOR_B | VECTOR_C;

const LANE_COUNTS: [usize; 5] = [8, 64, 256, 1024, 4096];
/// The longest repeat count runs about this long, so setup is a small part of every fit.
const TARGET_NS: f64 = 4_000_000.0;
/// Repeat counts per fit, as fractions of the calibrated maximum.
const REPEAT_STEPS: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
const ROUNDS: usize = 40;
const WARMUP_ROUNDS: usize = 3;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

struct Kernel {
    name: &'static str,
    formula: &'static str,
    width: &'static str,
    op_word: u64,
    bpf_pattern: u8,
    /// lane 0 of `c` for kernels that broadcast it
    c0: Option<u128>,
}

impl Kernel {
    fn wide(&self) -> bool {
        self.op_word & FLAG_WIDE as u64 != 0
    }

    /// The BPF loops read only the width flag, which sizes the regions.
    fn bpf_op_word(&self) -> u64 {
        self.op_word & FLAG_WIDE as u64
    }
}

fn kernels() -> Vec<Kernel> {
    let mut kernels = Vec::new();
    for (width, wide, patterns) in [
        ("u64", 0, [PATTERN_BPF_ADD, PATTERN_BPF_MUL128_SHR, PATTERN_BPF_MUL_DIV, PATTERN_BPF_LERP52]),
        ("u128", FLAG_WIDE, [PATTERN_BPF_ADD_U128, PATTERN_BPF_MUL128_SHR_U128, PATTERN_BPF_MUL_DIV_U128, PATTERN_BPF_LERP52_U128]),
    ] {
        kernels.extend([
            Kernel { name: "add", formula: "a + b", width, op_word: encode(wide | VECTOR_ALL, &[A, B, ADD]), bpf_pattern: patterns[0], c0: None },
            Kernel {
                name: "mul128_shr",
                formula: "(a * b) >> 26",
                width,
                op_word: encode(wide | VECTOR_A | VECTOR_B, &[A, B, C, MUL128_SHR]),
                bpf_pattern: patterns[1],
                c0: Some(26),
            },
            Kernel {
                name: "mul_div_1e9",
                formula: "a * b / 1e9",
                width,
                op_word: encode(wide | VECTOR_A | VECTOR_B, &[A, B, C, MUL128_DIV]),
                bpf_pattern: patterns[2],
                c0: Some(1_000_000_000),
            },
            Kernel {
                name: "lerp52",
                formula: "a + ((b - a) * f >> 52)",
                width,
                op_word: encode(wide | VECTOR_ALL, &[A, B, A, SUB, C, MUL_HI52, ADD]),
                bpf_pattern: patterns[3],
                c0: None,
            },
        ]);
    }
    kernels
}

/// One (kernel, width, implementation, lanes) series, with its account already in the store.
struct Series {
    kernel: &'static str,
    width: &'static str,
    implementation: &'static str,
    lanes: usize,
    pattern: u8,
    op_word: u64,
    buffers: Pubkey,
    cu_per_repeat: u64,
    repeats: Vec<u64>,
    /// samples[step] = wall ns per call
    samples: Vec<Vec<f64>>,
}

struct Bench {
    seashell: Seashell,
    program_id: Pubkey,
}

impl Bench {
    fn new() -> Option<Self> {
        let root = try_find_workspace_root().unwrap();
        let deploy_dir = root.join("programs/sol-math/target/deploy");
        if !deploy_dir.join("sol_math.so").is_file() {
            eprintln!("programs/sol-math/target/deploy/sol_math.so not built (needs the repeat count); skipping");
            return None;
        }
        let mut seashell = Seashell::new();
        // the longest BPF series runs far past the per-instruction default
        seashell.compute_budget.compute_unit_limit = 1 << 40;
        unsafe { std::env::set_var("SBF_OUT_DIR", deploy_dir.to_str().unwrap()) }
        let program_id = Pubkey::new_unique();
        seashell.load_program_from_environment("sol_math", program_id).unwrap();
        Some(Self { seashell, program_id })
    }

    fn instruction(&self, buffers: Pubkey, pattern: u8, op_word: u64, lanes: usize, repeats: u64) -> Instruction {
        let mut data = vec![0u8; 32];
        data[0] = pattern;
        data[8..16].copy_from_slice(&op_word.to_le_bytes());
        data[16..24].copy_from_slice(&(lanes as u64).to_le_bytes());
        data[24..32].copy_from_slice(&repeats.to_le_bytes());
        Instruction { program_id: self.program_id, accounts: vec![AccountMeta::new(buffers, false)], data }
    }

    /// Returns (wall ns, CU, output region).
    fn run(&self, buffers: Pubkey, pattern: u8, op_word: u64, lanes: usize, repeats: u64) -> (f64, u64, Vec<u8>) {
        let instruction = self.instruction(buffers, pattern, op_word, lanes, repeats);
        let start = Instant::now();
        let result = self.seashell.process_instruction(instruction);
        let elapsed = start.elapsed().as_nanos() as f64;
        assert!(result.error.is_none(), "pattern {pattern} lanes {lanes} repeats {repeats}: {:?}", result.error);
        let data = &result.post_execution_accounts.iter().find(|(key, _)| *key == buffers).unwrap().1.data;
        let lane_bytes = if op_word & FLAG_WIDE as u64 != 0 { 16 } else { 8 };
        (elapsed, result.compute_units_consumed, data[3 * lanes * lane_bytes..].to_vec())
    }

    fn set_buffers(&self, lanes: usize, wide: bool, c0: Option<u128>, rng: &mut Rng) -> Pubkey {
        // u64 lanes keep 52 bits, u128 lanes 116
        let mut draw = |shift: u32| -> u128 {
            if wide { ((rng.next() as u128) << 64 | rng.next() as u128) >> shift } else { (rng.next() >> shift) as u128 }
        };
        let mut a: Vec<u128> = (0..lanes).map(|_| draw(12)).collect();
        let mut b: Vec<u128> = (0..lanes).map(|_| draw(12) | 1).collect();
        // a <= b: MUL_HI52 sees only 52 bits, so the u64 lerp52's two forms agree only when b - a does not wrap
        for (low, high) in a.iter_mut().zip(b.iter_mut()) {
            if *low > *high {
                std::mem::swap(low, high);
            }
        }
        let mut c: Vec<u128> = (0..lanes).map(|_| draw(14) | 1).collect();
        if let Some(c0) = c0 {
            c[0] = c0;
        }
        let lane_bytes = if wide { 16 } else { 8 };
        let mut data: Vec<u8> =
            [a, b, c].iter().flatten().flat_map(|value| value.to_le_bytes()[..lane_bytes].to_vec()).collect();
        data.resize(4 * lanes * lane_bytes, 0);
        let buffers = Pubkey::new_unique();
        self.seashell.set_account(
            buffers,
            Account { lamports: 1_000_000_000, data, owner: self.program_id, executable: false, rent_epoch: 0 },
        );
        buffers
    }

    fn median_ns(&self, buffers: Pubkey, pattern: u8, op_word: u64, lanes: usize, repeats: u64) -> f64 {
        let mut times: Vec<f64> = (0..5).map(|_| self.run(buffers, pattern, op_word, lanes, repeats).0).collect();
        median(&mut times)
    }

    /// The largest repeat count whose call runs about `TARGET_NS` past the zero-repeat call.
    fn calibrate(&self, buffers: Pubkey, pattern: u8, op_word: u64, lanes: usize) -> u64 {
        let base = self.median_ns(buffers, pattern, op_word, lanes, 0);
        let mut repeats = 1u64;
        loop {
            let extra = self.median_ns(buffers, pattern, op_word, lanes, repeats) - base;
            if extra >= TARGET_NS / 8.0 {
                return ((repeats as f64 * TARGET_NS / extra).ceil() as u64).max(4);
            }
            repeats *= 4;
        }
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let middle = values.len() / 2;
    if values.len() % 2 == 1 { values[middle] } else { (values[middle - 1] + values[middle]) / 2.0 }
}

/// Least squares `y = intercept + slope * x`; returns (slope, intercept, r²).
fn fit(points: &[(f64, f64)]) -> (f64, f64, f64) {
    let n = points.len() as f64;
    let mean_x = points.iter().map(|p| p.0).sum::<f64>() / n;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|p| (p.0 - mean_x).powi(2)).sum();
    let sxy: f64 = points.iter().map(|p| (p.0 - mean_x) * (p.1 - mean_y)).sum();
    let syy: f64 = points.iter().map(|p| (p.1 - mean_y).powi(2)).sum();
    let slope = sxy / sxx;
    (slope, mean_y - slope * mean_x, sxy * sxy / (sxx * syy))
}

/// Mean of the middle 80%. Call times come in two clusters about 5% apart, picked at random
/// per call, so a median flips between them and a trimmed mean does not.
fn trimmed_mean(values: &mut [f64]) -> f64 {
    values.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let cut = values.len() / 10;
    let kept = &values[cut..values.len() - cut];
    kept.iter().sum::<f64>() / kept.len() as f64
}

fn minimum(values: &mut [f64]) -> f64 {
    values.iter().cloned().fold(f64::INFINITY, f64::min)
}

/// Slope from the per-step trimmed means, a 95% interval from resampling each step's samples,
/// and the slope from per-step minimums as a cross-check.
fn slope_with_interval(series: &Series, rng: &mut Rng) -> (f64, f64, f64, f64, f64, f64) {
    let per_step = |samples: &[Vec<f64>], estimate: fn(&mut [f64]) -> f64| -> Vec<(f64, f64)> {
        series.repeats.iter().zip(samples).map(|(r, s)| (*r as f64, estimate(&mut s.clone()))).collect()
    };
    let means = |samples: &[Vec<f64>]| per_step(samples, trimmed_mean);
    let (slope, intercept, r2) = fit(&means(&series.samples));
    let minimum_slope = fit(&per_step(&series.samples, minimum)).0;
    let mut slopes: Vec<f64> = (0..400)
        .map(|_| {
            let resampled: Vec<Vec<f64>> = series
                .samples
                .iter()
                .map(|s| (0..s.len()).map(|_| s[(rng.next() % s.len() as u64) as usize]).collect())
                .collect();
            fit(&means(&resampled)).0
        })
        .collect();
    slopes.sort_by(|x, y| x.partial_cmp(y).unwrap());
    (slope, intercept, r2, slopes[10], slopes[389], minimum_slope)
}

/// The BPF side of the comparison has to be the JIT, not the interpreter: the runtime counts
/// every invocation of the program's cache entry by the mode it actually ran in.
#[test]
fn bpf_runs_under_the_jit() {
    let Some(bench) = Bench::new() else { return };
    let mut rng = Rng(1);
    let entry = bench.seashell.accounts_db.programs.find(&bench.program_id).expect("program in cache");
    let count = |field: &std::sync::atomic::AtomicU64| field.load(std::sync::atomic::Ordering::Relaxed);
    let (jit_before, interpreted_before) = (count(&entry.stats.jit_invocations), count(&entry.stats.interpreted_invocations));
    let mut calls = 0;
    for kernel in kernels() {
        let buffers = bench.set_buffers(64, kernel.wide(), kernel.c0, &mut rng);
        bench.run(buffers, PATTERN_SOL_MATH, kernel.op_word, 64, 2);
        bench.run(buffers, kernel.bpf_pattern, kernel.bpf_op_word(), 64, 2);
        calls += 2;
    }
    let jit = count(&entry.stats.jit_invocations) - jit_before;
    let interpreted = count(&entry.stats.interpreted_invocations) - interpreted_before;
    println!("{calls} calls: {jit} jit, {interpreted} interpreted");
    assert_eq!((jit, interpreted), (calls, 0), "every call should run under the jit");
}

#[test]
#[ignore = "benchmark: run with --release --ignored"]
fn sol_math_vs_bpf_wall_time() {
    let Some(bench) = Bench::new() else { return };
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    println!("host path = {}", solana_syscalls::math::host_vector_path());
    if cfg!(debug_assertions) {
        println!("WARNING: debug build; numbers are meaningless, use --release");
    }

    let mut all: Vec<Series> = Vec::new();
    for kernel in kernels() {
        for lanes in LANE_COUNTS {
            let buffers = bench.set_buffers(lanes, kernel.wide(), kernel.c0, &mut rng);
            // the two implementations must agree before their times mean anything
            let (_, _, fast) = bench.run(buffers, PATTERN_SOL_MATH, kernel.op_word, lanes, 1);
            let (_, _, slow) = bench.run(buffers, kernel.bpf_pattern, kernel.bpf_op_word(), lanes, 1);
            assert_eq!(fast, slow, "{} {} lanes {lanes}: sol_math and bpf disagree", kernel.name, kernel.width);
            for (implementation, pattern, op_word) in
                [("sol_math", PATTERN_SOL_MATH, kernel.op_word), ("bpf", kernel.bpf_pattern, kernel.bpf_op_word())]
            {
                let (_, cu1, _) = bench.run(buffers, pattern, op_word, lanes, 1);
                let (_, cu3, _) = bench.run(buffers, pattern, op_word, lanes, 3);
                // CU linear in repeats means the loop really ran every time (nothing hoisted)
                let (_, cu5, _) = bench.run(buffers, pattern, op_word, lanes, 5);
                assert_eq!(cu5 - cu3, cu3 - cu1, "{} {implementation}: CU not linear in repeats", kernel.name);
                let calibrating = Instant::now();
                let max_repeats = bench.calibrate(buffers, pattern, op_word, lanes);
                eprintln!(
                    "calibrated {} {} {implementation} lanes {lanes}: {} CU/repeat, {max_repeats} repeats ({:.2}s)",
                    kernel.name,
                    kernel.width,
                    (cu3 - cu1) / 2,
                    calibrating.elapsed().as_secs_f64()
                );
                let repeats: Vec<u64> = REPEAT_STEPS.iter().map(|f| (f * max_repeats as f64).round() as u64).collect();
                all.push(Series {
                    kernel: kernel.name,
                    width: kernel.width,
                    implementation,
                    lanes,
                    pattern,
                    op_word,
                    buffers,
                    cu_per_repeat: (cu3 - cu1) / 2,
                    samples: vec![Vec::new(); repeats.len()],
                    repeats,
                });
            }
        }
    }
    // the repeat loop's own cost, left in every slope: whether LLVM keeps the empty noop loop
    // at all depends on the build, so it is reported, not subtracted
    let noop_buffers = bench.set_buffers(64, false, None, &mut rng);
    let (_, noop_cu1, _) = bench.run(noop_buffers, PATTERN_NOOP, 0, 64, 1);
    let (_, noop_cu9, _) = bench.run(noop_buffers, PATTERN_NOOP, 0, 64, 9);
    println!("repeat loop overhead: {} CU per repeat (noop pattern)", (noop_cu9 - noop_cu1) / 8);

    // shuffled rounds over every point
    let mut points: Vec<(usize, usize)> =
        (0..all.len()).flat_map(|series| (0..REPEAT_STEPS.len()).map(move |step| (series, step))).collect();
    let started = Instant::now();
    for round in 0..WARMUP_ROUNDS + ROUNDS {
        for i in (1..points.len()).rev() {
            points.swap(i, (rng.next() % (i as u64 + 1)) as usize);
        }
        for &(index, step) in &points {
            let series = &all[index];
            let (ns, _, _) = bench.run(series.buffers, series.pattern, series.op_word, series.lanes, series.repeats[step]);
            if round >= WARMUP_ROUNDS {
                all[index].samples[step].push(ns);
            }
        }
    }
    println!("sampled {} points x {ROUNDS} rounds in {:.1}s", points.len(), started.elapsed().as_secs_f64());

    let mut fits_csv =
        String::from("kernel,width,impl,lanes,cu_per_repeat,max_repeats,slope_ns,slope_lo_ns,slope_hi_ns,slope_min_ns,intercept_ns,r2\n");
    let mut samples_csv = String::from("kernel,width,impl,lanes,repeats,ns\n");
    let mut fitted: Vec<(&str, &str, &str, usize, u64, f64, f64, f64, f64)> = Vec::new();
    for series in &all {
        let (slope, intercept, r2, lo, hi, minimum_slope) = slope_with_interval(series, &mut rng);
        writeln!(
            fits_csv,
            "{},{},{},{},{},{},{slope:.3},{lo:.3},{hi:.3},{minimum_slope:.3},{intercept:.1},{r2:.6}",
            series.kernel,
            series.width,
            series.implementation,
            series.lanes,
            series.cu_per_repeat,
            series.repeats.last().unwrap()
        )
        .unwrap();
        for (repeats, samples) in series.repeats.iter().zip(&series.samples) {
            for ns in samples {
                writeln!(
                    samples_csv,
                    "{},{},{},{},{repeats},{ns:.0}",
                    series.kernel, series.width, series.implementation, series.lanes
                )
                .unwrap();
            }
        }
        fitted.push((
            series.kernel,
            series.width,
            series.implementation,
            series.lanes,
            series.cu_per_repeat,
            slope,
            intercept,
            r2,
            minimum_slope,
        ));
    }

    let intercepts: Vec<f64> = fitted.iter().map(|f| f.6 / 1000.0).collect();
    println!(
        "\nsetup (fit intercept, everything but the kernel): {:.1}..{:.1} us per call",
        intercepts.iter().cloned().fold(f64::INFINITY, f64::min),
        intercepts.iter().cloned().fold(0.0, f64::max)
    );
    println!(
        "\n| {:<24} | {:>5} | {:>5} | {:>12} | {:>12} | {:>8} | {:>8} | {:>9} | {:>9} | {:>7} | {:>7} | {:>7} |",
        "formula", "width", "lanes", "sol_math ns", "bpf ns", "sm CU", "bpf CU", "ns/lane sm", "ns/lane bpf", "speedup", "min fit", "CU ratio"
    );
    println!("|{}|{}|{}:|{}:|{}:|{}:|{}:|{}:|{}:|{}:|{}:|{}:|", "-".repeat(26), "-".repeat(7), "-".repeat(6), "-".repeat(13), "-".repeat(13), "-".repeat(9), "-".repeat(9), "-".repeat(10), "-".repeat(10), "-".repeat(8), "-".repeat(8), "-".repeat(8));
    for kernel in kernels() {
        for lanes in LANE_COUNTS {
            // (CU, slope, minimum slope)
            let find = |implementation: &str| {
                fitted
                    .iter()
                    .find(|f| f.0 == kernel.name && f.1 == kernel.width && f.2 == implementation && f.3 == lanes)
                    .map(|f| (f.4, f.5, f.8))
                    .unwrap()
            };
            let ((fast_cu, fast_ns, fast_min), (slow_cu, slow_ns, slow_min)) = (find("sol_math"), find("bpf"));
            println!(
                "| {:<24} | {:<5} | {lanes:>5} | {fast_ns:>12.1} | {slow_ns:>12.1} | {fast_cu:>8} | {slow_cu:>8} | {:>10.2} | {:>10.2} | {:>6.1}x | {:>6.1}x | {:>6.1}x |",
                kernel.formula,
                kernel.width,
                fast_ns / lanes as f64,
                slow_ns / lanes as f64,
                slow_ns / fast_ns,
                slow_min / fast_min,
                slow_cu as f64 / fast_cu as f64,
            );
        }
    }
    let worst_r2 = fitted.iter().map(|f| f.7).fold(f64::INFINITY, f64::min);
    println!("\nworst r² over all fits: {worst_r2:.5}");

    if let Ok(dir) = std::env::var("SOL_MATH_BENCH_OUT") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(format!("{dir}/fits.csv"), fits_csv).unwrap();
        std::fs::write(format!("{dir}/samples.csv"), samples_csv).unwrap();
        println!("wrote {dir}/fits.csv, {dir}/samples.csv");
    }
}
