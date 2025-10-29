use seashell::{try_find_workspace_root, Seashell};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

#[test]
fn test_create_account() {
    let mut seashell = Seashell::new_with_config(seashell::Config {
        memoize: true,
        allow_uninitialized_accounts: true,
    });
    let account_loader_out_dir = try_find_workspace_root()
        .unwrap()
        .join("programs/create-account/target/deploy");
    unsafe { std::env::set_var("SBF_OUT_DIR", account_loader_out_dir.to_str().unwrap()) }
    let program_id = Pubkey::from_str_const("create1111111111111111111111111111111111111");
    seashell
        .load_program_from_environment("create_account", program_id)
        .unwrap();

    seashell.enable_log_collector();

    let signer = Pubkey::new_unique();
    seashell.set_account(
        signer,
        Account {
            lamports: 10_000_000,
            data: vec![],
            owner: Pubkey::default(),
            executable: false,
            rent_epoch: 0,
        },
    );

    let new_account = Pubkey::find_program_address(&[b"test", signer.as_ref()], &program_id).0;

    // seashell.set_account(
    //     new_account,
    //     Account::default()
    // );

    let accounts = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new(new_account, false),
        AccountMeta::new_readonly(Pubkey::default(), false),
    ];

    let instruction = Instruction { program_id, accounts, data: vec![] };

    let result = seashell.process_instruction(instruction);
    assert!(result.error.is_none());

    let new_account_data = seashell.account(&new_account);
    assert_eq!(new_account_data.lamports, 7850880);
    assert_eq!(new_account_data.data.len(), 1000);
}
