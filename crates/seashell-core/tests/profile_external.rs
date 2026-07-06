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

#[test]
fn profile_create_account_variants() {
    let Ok(root) = std::env::var("PROFILING_ANCHOR_DIR") else {
        eprintln!("PROFILING_ANCHOR_DIR not set; skipping external profiling test");
        return;
    };

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

    for v in &variants {
        let mut seashell = Seashell::new_with_config(seashell::Config {
            memoize: true,
            allow_uninitialized_accounts_local: true,
            allow_uninitialized_accounts_fetched: true,
        });
        let deploy_dir = format!("{root}/{}/target/deploy", v.dir);
        unsafe { std::env::set_var("SBF_OUT_DIR", &deploy_dir) }
        seashell
            .load_program_from_environment(v.name, v.program_id)
            .unwrap_or_else(|e| panic!("load {} from {deploy_dir}: {e:?}", v.name));

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
        let profiler = seashell.profiler.clone().expect("profiler populated");
        assert!(
            result.error.is_none(),
            "{}: unexpected error: {:?}",
            v.name,
            result.error
        );
        assert_eq!(
            profiler.total_self_cu(),
            result.compute_units_consumed,
            "{}: profiler self-CU must equal consumed CU",
            v.name
        );

        let sym = seashell.symbolicator(&v.program_id).cloned().unwrap_or_default();
        eprintln!(
            "\n==== {} — {} CU across {} nodes (dwarf: {}) ====",
            v.name,
            result.compute_units_consumed,
            profiler.nodes.len(),
            sym.has_dwarf(),
        );
        for line in sym.folded_inlined(&profiler) {
            eprintln!("{line}");
        }

        let svg_path = flamegraph_dir.join(format!("{}.svg", v.name));
        let svg = std::fs::File::create(&svg_path).unwrap();
        sym.render_svg(
            &profiler,
            &format!("{} create account - CU flamegraph", v.dir),
            svg,
        )
        .expect("render flamegraph");
        eprintln!("wrote {}", svg_path.display());
    }
}
