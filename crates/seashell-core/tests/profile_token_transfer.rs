mod common;

use std::net::TcpListener;

use common::{latest_profile_json, render_quasar_svg, spl_token_account, TOKEN_ACCOUNT_RENT};
use seashell::symbolicate::Symbolicator;
use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

const AMOUNT: u64 = 250;

#[test]
fn profile_token_transfer() {
    let root = try_find_workspace_root().unwrap();
    let deploy_dir = root.join("programs/token-transfer/target/deploy");
    let unstripped = root.join("programs/token-transfer/target/sbpf-solana-solana/release/token_transfer.so");
    if !deploy_dir.join("token_transfer.so").is_file() {
        eprintln!("token_transfer.so not built; skipping");
        return;
    }

    let mut seashell = Seashell::new();
    seashell.enable_log_collector();
    unsafe { std::env::set_var("SBF_OUT_DIR", deploy_dir.to_str().unwrap()) }
    let program_id = Pubkey::new_unique();
    seashell
        .load_program_from_environment("token_transfer", program_id)
        .unwrap();

    let mint = Pubkey::new_unique();
    let authority = Pubkey::new_unique();
    let source = Pubkey::new_unique();
    let destination = Pubkey::new_unique();
    seashell.airdrop(authority, 1_000_000);
    seashell.set_account(
        source,
        Account {
            lamports: TOKEN_ACCOUNT_RENT,
            data: spl_token_account(&mint, &authority, 1_000, false),
            owner: seashell::spl::TOKEN_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    seashell.set_account(
        destination,
        Account {
            lamports: TOKEN_ACCOUNT_RENT,
            data: spl_token_account(&mint, &Pubkey::new_unique(), 0, false),
            owner: seashell::spl::TOKEN_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    let ixn = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(source, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new_readonly(seashell::spl::TOKEN_PROGRAM_ID, false),
        ],
        data: AMOUNT.to_le_bytes().to_vec(),
    };

    let result = seashell.profile_instruction(ixn);
    assert!(result.error.is_none(), "unexpected error: {:?}", result.error);
    let profiler = seashell.profiler.clone().expect("profiler populated");
    assert_eq!(profiler.total_self_cu(), result.compute_units_consumed);

    let post_destination = result
        .post_execution_accounts
        .iter()
        .find(|(pubkey, _)| *pubkey == destination)
        .unwrap()
        .1
        .clone();
    let received = u64::from_le_bytes(post_destination.data[64..72].try_into().unwrap());
    assert_eq!(received, AMOUNT);

    eprintln!(
        "\n=== token transfer via CPI: {} CU across {} nodes ===",
        result.compute_units_consumed,
        profiler.nodes.len(),
    );
    let fallback = Symbolicator::new();
    let syms = seashell.program_symbolicators(&fallback);
    for line in syms.folded_inlined(&profiler) {
        eprintln!("{line}");
    }

    let flamegraph_dir = root.join("target/flamegraphs");
    std::fs::create_dir_all(&flamegraph_dir).unwrap();
    let svg = std::fs::File::create(flamegraph_dir.join("token_transfer.svg")).unwrap();
    syms.render_svg(&profiler, "token transfer - CU flamegraph (seashell)", svg)
        .expect("render flamegraph");
    eprintln!("wrote {}", flamegraph_dir.join("token_transfer.svg").display());

    if unstripped.is_file() {
        let _suppress_quasar_server = TcpListener::bind("127.0.0.1:7777");
        eprintln!("\n==== quasar static profile: token_transfer ====");
        quasar_profile::run(quasar_profile::ProfileCommand {
            elf_path: Some(unstripped),
            diff_program: None,
            share: false,
            expand: false,
        });
        let json = latest_profile_json("token_transfer").expect("quasar profile json written");
        let svg_path = flamegraph_dir.join("quasar_token_transfer.svg");
        let static_cu = render_quasar_svg(
            &json,
            "token transfer - static code CU flamegraph (quasar)",
            &svg_path,
        );
        eprintln!("wrote {} ({static_cu} static CU)", svg_path.display());
    }
}
