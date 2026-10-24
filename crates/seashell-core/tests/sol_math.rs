//! `sol_math` end to end: the program in `programs/sol-math` calls the syscall, the output is
//! checked lane by lane against the host reference evaluator, and the CU is printed next to
//! the same arithmetic written as a BPF loop.

use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_syscalls::math::{
    encode, evaluate_u128, evaluate_u64, A, ADD, ARSH, B, C, DIV, LIT, LRSH, MAX, MIN, MUL128_DIV, MUL128_SHR,
    MUL_HI52, NEG, SELECT, SUB,
};

const PATTERN_NOOP: u8 = 0;
const PATTERN_SOL_MATH: u8 = 1;
const PATTERN_BPF_MUL128_SHR: u8 = 2;
const PATTERN_BPF_LERP52: u8 = 3;
const PATTERN_BPF_ADD: u8 = 4;
const PATTERN_BPF_MUL_DIV: u8 = 5;

const FLAG_WIDE: u8 = 0b0001;
const VECTOR_A: u8 = 0b0010;
const VECTOR_B: u8 = 0b0100;
const VECTOR_C: u8 = 0b1000;
const VECTOR_ALL: u8 = VECTOR_A | VECTOR_B | VECTOR_C;

const LANE_COUNT: usize = 64;

/// Seeded xorshift so every run sees the same inputs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

struct Harness {
    seashell: Seashell,
    program_id: Pubkey,
}

impl Harness {
    fn new() -> Option<Self> {
        let root = try_find_workspace_root().unwrap();
        // a fresh build if there is one, else the checked-in copy
        let deploy_dir = root.join("programs/sol-math/target/deploy");
        let fixture_dir = root.join("crates/seashell-core/tests/fixtures");
        let program_dir = if deploy_dir.join("sol_math.so").is_file() {
            deploy_dir
        } else if fixture_dir.join("sol_math.so").is_file() {
            fixture_dir
        } else {
            eprintln!("sol_math.so not built; skipping");
            return None;
        };
        let mut seashell = Seashell::new();
        seashell.enable_log_collector();
        unsafe { std::env::set_var("SBF_OUT_DIR", program_dir.to_str().unwrap()) }
        let program_id = Pubkey::new_unique();
        seashell.load_program_from_environment("sol_math", program_id).unwrap();
        Some(Self { seashell, program_id })
    }

    /// Runs one instruction; returns the CU consumed and the account data afterwards.
    fn run(&self, pattern: u8, op_word: u64, lane_count: usize, regions: [&[u8]; 3]) -> (u64, Vec<u8>) {
        let region_bytes = regions[0].len();
        assert!(regions.iter().all(|region| region.len() == region_bytes));
        let mut data = Vec::with_capacity(4 * region_bytes);
        for region in regions {
            data.extend_from_slice(region);
        }
        data.resize(4 * region_bytes, 0);

        let buffers = Pubkey::new_unique();
        self.seashell.set_account(
            buffers,
            Account { lamports: 1_000_000, data, owner: self.program_id, executable: false, rent_epoch: 0 },
        );
        let mut instruction_data = vec![0u8; 24];
        instruction_data[0] = pattern;
        instruction_data[8..16].copy_from_slice(&op_word.to_le_bytes());
        instruction_data[16..24].copy_from_slice(&(lane_count as u64).to_le_bytes());
        let instruction = Instruction {
            program_id: self.program_id,
            accounts: vec![AccountMeta::new(buffers, false)],
            data: instruction_data,
        };
        let result = self.seashell.process_instruction(instruction);
        assert!(result.error.is_none(), "pattern {pattern} op {op_word:#x}: {:?} logs: {:?}", result.error, self.seashell.logs());
        let post = result
            .post_execution_accounts
            .iter()
            .find(|(pubkey, _)| *pubkey == buffers)
            .expect("buffers account")
            .1
            .data
            .clone();
        (result.compute_units_consumed, post[3 * region_bytes..].to_vec())
    }
}

fn bytes_u64(values: &[u64]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn bytes_u128(values: &[u128]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn lanes_u64(bytes: &[u8]) -> Vec<u64> {
    bytes.chunks(8).map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap())).collect()
}

fn lanes_u128(bytes: &[u8]) -> Vec<u128> {
    bytes.chunks(16).map(|chunk| u128::from_le_bytes(chunk.try_into().unwrap())).collect()
}

/// Picks lane `lane` of a vector input or lane 0 of a broadcast one.
fn pick<T: Copy>(values: &[T], is_vector: bool, lane: usize) -> T {
    values[if is_vector { lane } else { 0 }]
}

#[test]
fn sol_math_matches_the_host_reference() {
    let Some(harness) = Harness::new() else { return };
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);

    // (name, flags, tokens): every u64 case runs on LANE_COUNT lanes
    let cases: [(&str, u8, &[u8]); 12] = [
        ("add", VECTOR_ALL, &[A, B, ADD]),
        ("sub", VECTOR_ALL, &[A, B, SUB]),
        ("min", VECTOR_ALL, &[A, B, MIN]),
        ("max", VECTOR_ALL, &[A, B, MAX]),
        ("lrsh_lit", VECTOR_ALL, &[A, LIT, 26, LRSH]),
        ("div", VECTOR_ALL, &[A, B, DIV]),
        ("mul_hi52", VECTOR_ALL, &[A, B, MUL_HI52]),
        ("mul128_shr", VECTOR_A | VECTOR_B, &[A, B, C, MUL128_SHR]),
        ("mul128_div", VECTOR_ALL, &[A, B, C, MUL128_DIV]),
        ("mul_div_1e9", VECTOR_A | VECTOR_B, &[A, B, C, MUL128_DIV]),
        ("lerp52", VECTOR_ALL, &[A, B, A, SUB, C, MUL_HI52, ADD]),
        ("select_gt0", VECTOR_ALL, &[A, NEG, LIT, 63, ARSH, B, C, SELECT]),
    ];

    let a: Vec<u64> = (0..LANE_COUNT).map(|_| rng.next() >> 12).collect();
    let b: Vec<u64> = (0..LANE_COUNT).map(|_| (rng.next() >> 12) | 1).collect();
    let mut c: Vec<u64> = (0..LANE_COUNT).map(|_| (rng.next() >> 14) | 1).collect();
    c[0] = 26; // the broadcast shift for mul128_shr
    let (a_bytes, b_bytes, c_bytes) = (bytes_u64(&a), bytes_u64(&b), bytes_u64(&c));
    let mut c_1e9 = c.clone();
    c_1e9[0] = 1_000_000_000;
    let c_1e9_bytes = bytes_u64(&c_1e9);

    let noop_cu = harness.run(PATTERN_NOOP, 0, LANE_COUNT, [&a_bytes, &b_bytes, &c_bytes]).0;
    let net = |cu: u64| cu.saturating_sub(noop_cu);
    let per_lane = |cu: u64| net(cu) as f64 / LANE_COUNT as f64;

    // every sol_math case, checked lane by lane; CU collected for the tables below
    let mut sol_math_cu: Vec<(&str, u64)> = Vec::new();
    for (name, flags, tokens) in cases {
        let word = encode(flags, tokens);
        let c_input = if name == "mul_div_1e9" { &c_1e9 } else { &c };
        let c_input_bytes = if name == "mul_div_1e9" { &c_1e9_bytes } else { &c_bytes };
        let (cu, output) = harness.run(PATTERN_SOL_MATH, word, LANE_COUNT, [&a_bytes, &b_bytes, c_input_bytes]);
        let output = lanes_u64(&output);
        for lane in 0..LANE_COUNT {
            let expected = evaluate_u64(
                word,
                pick(&a, flags & VECTOR_A != 0, lane),
                pick(&b, flags & VECTOR_B != 0, lane),
                pick(c_input, flags & VECTOR_C != 0, lane),
            )
            .unwrap();
            assert_eq!(output[lane], expected, "{name} lane {lane}");
        }
        sol_math_cu.push((name, cu));
    }
    let sol_math_net = |name: &str| net(sol_math_cu.iter().find(|(case, _)| *case == name).unwrap().1);

    // the same four formulas as plain BPF loops, checked the same way
    let (bpf_add_cu, output) = harness.run(PATTERN_BPF_ADD, 0, LANE_COUNT, [&a_bytes, &b_bytes, &c_bytes]);
    for (lane, value) in lanes_u64(&output).into_iter().enumerate() {
        assert_eq!(value, a[lane].wrapping_add(b[lane]), "bpf add lane {lane}");
    }
    let (bpf_mul128_shr_cu, output) = harness.run(PATTERN_BPF_MUL128_SHR, 0, LANE_COUNT, [&a_bytes, &b_bytes, &c_bytes]);
    for (lane, value) in lanes_u64(&output).into_iter().enumerate() {
        assert_eq!(value, ((a[lane] as u128 * b[lane] as u128) >> 26) as u64, "bpf mul128_shr lane {lane}");
    }
    let (bpf_mul_div_cu, output) = harness.run(PATTERN_BPF_MUL_DIV, 0, LANE_COUNT, [&a_bytes, &b_bytes, &c_1e9_bytes]);
    for (lane, value) in lanes_u64(&output).into_iter().enumerate() {
        assert_eq!(value, (a[lane] as u128 * b[lane] as u128 / 1_000_000_000) as u64, "bpf mul_div lane {lane}");
    }
    let (bpf_lerp_cu, output) = harness.run(PATTERN_BPF_LERP52, 0, LANE_COUNT, [&a_bytes, &b_bytes, &c_bytes]);
    for (lane, value) in lanes_u64(&output).into_iter().enumerate() {
        let expected = a[lane].wrapping_add(((b[lane].wrapping_sub(a[lane]) as u128 * c[lane] as u128) >> 52) as u64);
        assert_eq!(value, expected, "bpf lerp52 lane {lane}");
    }

    println!("\nlanes = {LANE_COUNT}, host path = {}", solana_syscalls::math::host_vector_path());
    println!("\nsame formula, sol_math vs a BPF loop (net CU, noop instruction subtracted)");
    println!("| {:<24} | {:>8} | {:>8} | {:>8} | {:>8} | {:>14} |", "formula", "sol_math", "per lane", "bpf", "per lane", "bpf / sol_math");
    println!("|{}|{}:|{}:|{}:|{}:|{}:|", "-".repeat(26), "-".repeat(9), "-".repeat(9), "-".repeat(9), "-".repeat(9), "-".repeat(15));
    for (formula, case, bpf_cu) in [
        ("a + b", "add", bpf_add_cu),
        ("(a * b) >> 26", "mul128_shr", bpf_mul128_shr_cu),
        ("a * b / 1e9", "mul_div_1e9", bpf_mul_div_cu),
        ("a + ((b - a) * f >> 52)", "lerp52", bpf_lerp_cu),
    ] {
        let fast = sol_math_net(case);
        println!(
            "| {formula:<24} | {fast:>8} | {:>8.2} | {:>8} | {:>8.2} | {:>13.1}x |",
            fast as f64 / LANE_COUNT as f64,
            net(bpf_cu),
            per_lane(bpf_cu),
            net(bpf_cu) as f64 / fast as f64,
        );
    }

    println!("\nevery sol_math case");
    println!("| {:<16} | {:>8} | {:>8} | {:>8} |", "case", "CU", "net CU", "per lane");
    println!("|{}|{}:|{}:|{}:|", "-".repeat(18), "-".repeat(9), "-".repeat(9), "-".repeat(9));
    let row = |name: &str, cu: u64| println!("| {name:<16} | {cu:>8} | {:>8} | {:>8.2} |", net(cu), per_lane(cu));
    row("noop", noop_cu);
    for (name, cu) in &sol_math_cu {
        row(name, *cu);
    }

    // partial vector chunks: on x86 this is the AVX-512 path against the scalar reference
    for lane_count in [1usize, 7, 9] {
        let region = |values: &[u64]| bytes_u64(&values[..lane_count]);
        for (name, flags, tokens) in cases {
            let word = encode(flags, tokens);
            let c_input = if name == "mul_div_1e9" { &c_1e9 } else { &c };
            let (_, output) = harness.run(PATTERN_SOL_MATH, word, lane_count, [&region(&a), &region(&b), &region(c_input)]);
            for (lane, value) in lanes_u64(&output).into_iter().enumerate() {
                let expected = evaluate_u64(
                    word,
                    pick(&a, flags & VECTOR_A != 0, lane),
                    pick(&b, flags & VECTOR_B != 0, lane),
                    pick(c_input, flags & VECTOR_C != 0, lane),
                )
                .unwrap();
                assert_eq!(value, expected, "{name} lane {lane} of {lane_count}");
            }
        }
    }

    // u128 lanes
    let wide_a: Vec<u128> = (0..LANE_COUNT).map(|_| (rng.next() as u128) << 64 | rng.next() as u128).collect();
    let wide_b: Vec<u128> = (0..LANE_COUNT).map(|_| (rng.next() as u128) << 40 | 1).collect();
    let wide_c: Vec<u128> = (0..LANE_COUNT).map(|_| (rng.next() >> 20) as u128 | 1).collect();
    let (wide_a_bytes, wide_b_bytes, wide_c_bytes) = (bytes_u128(&wide_a), bytes_u128(&wide_b), bytes_u128(&wide_c));
    let wide_cases: [(&str, &[u8]); 3] = [
        ("u128 add", &[A, B, ADD]),
        ("u128 div", &[A, B, DIV]),
        ("u128 mul128_div", &[A, B, C, MUL128_DIV]),
    ];
    for (name, tokens) in wide_cases {
        let word = encode(FLAG_WIDE | VECTOR_ALL, tokens);
        let (cu, output) = harness.run(PATTERN_SOL_MATH, word, LANE_COUNT, [&wide_a_bytes, &wide_b_bytes, &wide_c_bytes]);
        for (lane, value) in lanes_u128(&output).into_iter().enumerate() {
            assert_eq!(value, evaluate_u128(word, wide_a[lane], wide_b[lane], wide_c[lane]).unwrap(), "{name} lane {lane}");
        }
        row(name, cu);
    }
}

#[test]
fn sol_math_rejects_bad_sequences_and_zero_divisors() {
    let Some(harness) = Harness::new() else { return };
    let a = bytes_u64(&[1; 8]);
    let zeros = bytes_u64(&[0; 8]);

    for (what, word) in [
        ("empty", encode(VECTOR_ALL, &[])),
        ("two values left", encode(VECTOR_ALL, &[A, B])),
        ("underflow", encode(VECTOR_ALL, &[ADD])),
    ] {
        let result = harness.try_run(PATTERN_SOL_MATH, word, 8, [&a, &a, &a]);
        assert!(result.is_err(), "{what} should fail");
    }
    let divide = encode(VECTOR_ALL, &[A, B, DIV]);
    assert!(harness.try_run(PATTERN_SOL_MATH, divide, 8, [&a, &zeros, &a]).is_err(), "divide by zero should fail");
    assert!(harness.try_run(PATTERN_SOL_MATH, divide, 8, [&a, &a, &a]).is_ok());
}

impl Harness {
    /// Like `run`, but reports failure instead of panicking.
    fn try_run(&self, pattern: u8, op_word: u64, lane_count: usize, regions: [&[u8]; 3]) -> Result<(), String> {
        let region_bytes = regions[0].len();
        let mut data = Vec::with_capacity(4 * region_bytes);
        for region in regions {
            data.extend_from_slice(region);
        }
        data.resize(4 * region_bytes, 0);
        let buffers = Pubkey::new_unique();
        self.seashell.set_account(
            buffers,
            Account { lamports: 1_000_000, data, owner: self.program_id, executable: false, rent_epoch: 0 },
        );
        let mut instruction_data = vec![0u8; 24];
        instruction_data[0] = pattern;
        instruction_data[8..16].copy_from_slice(&op_word.to_le_bytes());
        instruction_data[16..24].copy_from_slice(&(lane_count as u64).to_le_bytes());
        let result = self.seashell.process_instruction(Instruction {
            program_id: self.program_id,
            accounts: vec![AccountMeta::new(buffers, false)],
            data: instruction_data,
        });
        match result.error {
            None => Ok(()),
            Some(error) => Err(format!("{error:?}")),
        }
    }
}
