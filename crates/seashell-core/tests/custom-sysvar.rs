use {
    seashell::{try_find_workspace_root, Seashell},
    serde::{Deserialize, Serialize},
    solana_instruction::Instruction,
    solana_pubkey::Pubkey,
    solana_sysvar::{Sysvar, SysvarSerialize},
    solana_sysvar_id::SysvarId,
};

const TEST_SYSVAR_ID: Pubkey = Pubkey::new_from_array([42; 32]);
const EXPECTED_VALUE: u64 = 0x0123_4567_89ab_cdef;

#[derive(Debug, Default, Deserialize, Serialize)]
struct TestSysvar {
    value: u64,
}

impl SysvarId for TestSysvar {
    fn id() -> Pubkey {
        TEST_SYSVAR_ID
    }

    fn check_id(pubkey: &Pubkey) -> bool {
        pubkey == &TEST_SYSVAR_ID
    }
}

impl Sysvar for TestSysvar {}
impl SysvarSerialize for TestSysvar {}

#[test]
fn test_custom_sysvar() {
    let mut seashell = Seashell::new();
    let program_out_dir = try_find_workspace_root()
        .unwrap()
        .join("programs/custom-sysvar/target/deploy");
    unsafe { std::env::set_var("SBF_OUT_DIR", program_out_dir) }

    let program_id = Pubkey::new_unique();
    seashell
        .load_program_from_environment("custom_sysvar", program_id)
        .unwrap();
    seashell.register_custom_sysvar(&TestSysvar {
        value: EXPECTED_VALUE,
    });

    let result = seashell.process_instruction(Instruction {
        program_id,
        accounts: Vec::new(),
        data: Vec::new(),
    });

    assert_eq!(result.error, None);
}
