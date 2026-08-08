mod common;

use std::net::TcpListener;
use std::path::PathBuf;

use common::{latest_profile_json, render_quasar_svg};

use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

const ANCHOR_CREATE_SYSTEM_ACCOUNT: [u8; 8] = [67, 217, 132, 246, 135, 232, 191, 81];

struct Variant {
    dir: &'static str,
    name: &'static str,
    program_id: Pubkey,
    instruction_data: Vec<u8>,
}

fn dynamic_cu(root: &str, v: &Variant) -> u64 {
    let mut seashell = Seashell::new_with_config(seashell::Config {
        memoize: true,
        allow_uninitialized_accounts_local: true,
        allow_uninitialized_accounts_fetched: true,
    });
    unsafe { std::env::set_var("SBF_OUT_DIR", format!("{root}/{}/target/deploy", v.dir)) }
    seashell
        .load_program_from_environment(v.name, v.program_id)
        .unwrap();

    let payer = Pubkey::new_unique();
    seashell.set_account(
        payer,
        Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: Pubkey::default(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let new_account = Pubkey::new_unique();

    let instruction = Instruction {
        program_id: v.program_id,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(new_account, true),
            AccountMeta::new_readonly(Pubkey::default(), false),
        ],
        data: v.instruction_data.clone(),
    };

    let result = seashell.profile_instruction(instruction);
    assert!(
        result.error.is_none(),
        "{}: unexpected error: {:?}",
        v.name,
        result.error
    );
    result.compute_units_consumed
}

#[test]
fn profile_quasar_static_vs_dynamic() {
    let Ok(root) = std::env::var("PROFILING_ANCHOR_DIR") else {
        eprintln!("PROFILING_ANCHOR_DIR not set; skipping quasar comparison test");
        return;
    };

    let _suppress_quasar_server = TcpListener::bind("127.0.0.1:7777");

    let variants = [
        Variant {
            dir: "anchor",
            name: "profile",
            program_id: Pubkey::from_str_const("Bench11111111111111111111111111111111111111"),
            instruction_data: ANCHOR_CREATE_SYSTEM_ACCOUNT.to_vec(),
        },
        Variant {
            dir: "native",
            name: "create_account_native",
            program_id: Pubkey::new_unique(),
            instruction_data: vec![],
        },
        Variant {
            dir: "pinocchio",
            name: "pinocchio_create_account",
            program_id: Pubkey::new_unique(),
            instruction_data: vec![0; 9],
        },
    ];

    let flamegraph_dir = try_find_workspace_root().unwrap().join("target/flamegraphs");
    std::fs::create_dir_all(&flamegraph_dir).unwrap();

    let mut rows = Vec::new();
    for v in &variants {
        let elf = PathBuf::from(format!(
            "{root}/{}/target/sbpf-solana-solana/release/{}.so",
            v.dir, v.name
        ));
        assert!(elf.is_file(), "unstripped elf missing: {}", elf.display());

        eprintln!("\n==== quasar static profile: {} ====", v.name);
        quasar_profile::run(quasar_profile::ProfileCommand {
            elf_path: Some(elf.clone()),
            diff_program: None,
            share: false,
            expand: false,
        });

        let json = latest_profile_json(v.name).expect("quasar profile json written");
        let svg_path = flamegraph_dir.join(format!("quasar_{}.svg", v.name));
        let static_cu = render_quasar_svg(
            &json,
            &format!("{} create account - static code CU flamegraph (quasar)", v.dir),
            &svg_path,
        );
        eprintln!("wrote {}", svg_path.display());

        let dynamic = dynamic_cu(&root, v);
        rows.push((v.name, static_cu, dynamic));
    }

    if let Ok(raydium_dir) = std::env::var("RAYDIUM_CPMM_DIR") {
        let elf = PathBuf::from(format!(
            "{raydium_dir}/target/sbpf-solana-solana/release/raydium_cp_swap.so"
        ));
        if elf.is_file() {
            eprintln!("\n==== quasar static profile: raydium_cp_swap ====");
            quasar_profile::run(quasar_profile::ProfileCommand {
                elf_path: Some(elf),
                diff_program: None,
                share: false,
                expand: false,
            });
            let json = latest_profile_json("raydium_cp_swap").expect("quasar profile json written");
            let svg_path = flamegraph_dir.join("quasar_raydium_cp_swap.svg");
            let static_cu = render_quasar_svg(
                &json,
                "raydium cpmm swap - static code CU flamegraph (quasar)",
                &svg_path,
            );
            eprintln!("wrote {} ({static_cu} static CU)", svg_path.display());
        }
    }

    eprintln!("\n==== comparison: quasar static (code CU) vs seashell dynamic (executed CU) ====");
    eprintln!(
        "{:<26} {:>18} {:>20}",
        "program", "static (all insns)", "dynamic (executed)"
    );
    for (name, static_cu, dynamic) in &rows {
        eprintln!("{:<26} {:>18} {:>20}", name, static_cu, dynamic);
    }
}
