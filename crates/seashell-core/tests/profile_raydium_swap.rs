use seashell::symbolicate::Symbolicator;
use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

const RAYDIUM_CPMM: Pubkey =
    Pubkey::from_str_const("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
const TOKEN_PROGRAM: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

const POOL: Pubkey = Pubkey::from_str_const("Q2sPHPdUWFMg7M7wwrQKLrn619cAucfRsmhVJffodSp");
const AMM_CONFIG: Pubkey = Pubkey::from_str_const("D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2");
const TOKEN_0_VAULT: Pubkey =
    Pubkey::from_str_const("HgNPDD8bpbSrGyHegiCT5xrYxHTfwLfZydwGkjNCJRKA");
const TOKEN_1_VAULT: Pubkey =
    Pubkey::from_str_const("9xsCiNwYQXM3ZeHFSVj9JQdP1vREJREpN23f6wvxA1ty");
const WSOL_MINT: Pubkey = Pubkey::from_str_const("So11111111111111111111111111111111111111112");
const TOKEN_1_MINT: Pubkey =
    Pubkey::from_str_const("Dz9mQ9NzkBcCsuGPFJ3r1bS4wgqKMHBPiVuniW8Mbonk");
const OBSERVATION: Pubkey = Pubkey::from_str_const("4UdSz2kMddtX4woMmdgkWg75fdBP8FgYwqfkh4ri7mnD");

const SWAP_BASE_INPUT: [u8; 8] = [143, 190, 90, 218, 196, 30, 51, 222];

const AMOUNT_IN: u64 = 1_000_000_000;
const TOKEN_ACCOUNT_RENT: u64 = 2_039_280;

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn spl_token_account(mint: &Pubkey, owner: &Pubkey, amount: u64, native: bool) -> Vec<u8> {
    let mut data = vec![0u8; 165];
    data[0..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    if native {
        data[109..113].copy_from_slice(&1u32.to_le_bytes());
        data[113..121].copy_from_slice(&TOKEN_ACCOUNT_RENT.to_le_bytes());
    }
    data
}

#[test]
fn profile_raydium_swap_via_cpi() {
    let Ok(raydium_dir) = std::env::var("RAYDIUM_CPMM_DIR") else {
        eprintln!("RAYDIUM_CPMM_DIR not set; skipping raydium swap profiling test");
        return;
    };
    let root = try_find_workspace_root().unwrap();

    let mut seashell = Seashell::new_with_config(seashell::Config {
        memoize: true,
        allow_uninitialized_accounts_local: true,
        allow_uninitialized_accounts_fetched: true,
    });
    seashell.load_spl();
    seashell.enable_log_collector();

    unsafe {
        std::env::set_var(
            "SBF_OUT_DIR",
            format!("{raydium_dir}/target/deploy"),
        )
    }
    seashell
        .load_program_from_environment("raydium_cp_swap", RAYDIUM_CPMM)
        .unwrap();
    let caller_id = Pubkey::new_unique();
    unsafe {
        std::env::set_var(
            "SBF_OUT_DIR",
            root.join("programs/cpi-caller/target/deploy").to_str().unwrap(),
        )
    }
    seashell.load_program_from_environment("cpi_caller", caller_id).unwrap();

    let fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("crates/seashell-core/tests/fixtures/raydium_cpmm_pool.json"))
            .unwrap(),
    )
    .unwrap();
    for (pubkey, account) in fixture.as_object().unwrap() {
        if pubkey.starts_with('_') {
            continue;
        }
        seashell.set_account(
            Pubkey::from_str_const(pubkey),
            Account {
                lamports: account["lamports"].as_u64().unwrap(),
                data: hex_decode(account["data_hex"].as_str().unwrap()),
                owner: Pubkey::from_str_const(account["owner"].as_str().unwrap()),
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let clock = &fixture["_clock"];
    seashell.warp(
        clock["slot"].as_u64().unwrap(),
        clock["timestamp"].as_u64().unwrap() + 5,
    );

    let payer = Pubkey::new_unique();
    seashell.airdrop(payer, 10_000_000_000);
    let user_wsol = Pubkey::new_unique();
    seashell.set_account(
        user_wsol,
        Account {
            lamports: TOKEN_ACCOUNT_RENT + AMOUNT_IN,
            data: spl_token_account(&WSOL_MINT, &payer, AMOUNT_IN, true),
            owner: TOKEN_PROGRAM,
            executable: false,
            rent_epoch: 0,
        },
    );
    let user_token_1 = Pubkey::new_unique();
    seashell.set_account(
        user_token_1,
        Account {
            lamports: TOKEN_ACCOUNT_RENT,
            data: spl_token_account(&TOKEN_1_MINT, &payer, 0, false),
            owner: TOKEN_PROGRAM,
            executable: false,
            rent_epoch: 0,
        },
    );

    let authority = Pubkey::find_program_address(&[b"vault_and_lp_mint_auth_seed"], &RAYDIUM_CPMM).0;
    let mut data = SWAP_BASE_INPUT.to_vec();
    data.extend_from_slice(&AMOUNT_IN.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());

    let ixn = Instruction {
        program_id: caller_id,
        accounts: vec![
            AccountMeta::new_readonly(RAYDIUM_CPMM, false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new_readonly(AMM_CONFIG, false),
            AccountMeta::new(POOL, false),
            AccountMeta::new(user_wsol, false),
            AccountMeta::new(user_token_1, false),
            AccountMeta::new(TOKEN_0_VAULT, false),
            AccountMeta::new(TOKEN_1_VAULT, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new_readonly(WSOL_MINT, false),
            AccountMeta::new_readonly(TOKEN_1_MINT, false),
            AccountMeta::new(OBSERVATION, false),
        ],
        data,
    };

    let result = seashell.profile_instruction(ixn);
    let profiler = seashell.profiler.clone().expect("profiler populated");
    if let Some(logs) = seashell.logs() {
        for log in &logs {
            eprintln!("log: {log}");
        }
    }
    assert!(result.error.is_none(), "swap failed: {:?}", result.error);
    assert_eq!(
        profiler.total_self_cu(),
        result.compute_units_consumed,
        "conservation across caller -> raydium -> token program",
    );

    let out = seashell.account(&user_token_1);
    let received = u64::from_le_bytes(out.data[64..72].try_into().unwrap());
    assert!(received > 0, "no tokens received");
    eprintln!(
        "\n=== raydium swap_base_input via CPI: {} CU across {} nodes, received {} tokens for 1 SOL ===",
        result.compute_units_consumed,
        profiler.nodes.len(),
        received,
    );

    let fallback = Symbolicator::new();
    let syms = seashell.program_symbolicators(&fallback);
    for line in syms.folded_inlined(&profiler) {
        eprintln!("{line}");
    }

    let out_dir = root.join("target/flamegraphs");
    std::fs::create_dir_all(&out_dir).unwrap();
    let svg = std::fs::File::create(out_dir.join("raydium_swap.svg")).unwrap();
    syms.render_svg(
        &profiler,
        "raydium cpmm swap_base_input via CPI - CU flamegraph (seashell)",
        svg,
    )
    .expect("render flamegraph");
    eprintln!("wrote {}", out_dir.join("raydium_swap.svg").display());
}
