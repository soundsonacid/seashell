#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use pinocchio::account_info::AccountInfo;
    use pinocchio::pubkey::Pubkey;
    use pinocchio::pubkey::find_program_address;
    use pinocchio::entrypoint;
    use pinocchio::ProgramResult;
    use pinocchio::signer;
    use pinocchio::sysvars::rent::Rent;
    use pinocchio::sysvars::Sysvar;

    use pinocchio_pubkey::pubkey;

    use pinocchio_system::instructions::CreateAccount;

    entrypoint!(process_instruction);

    const PROGRAM_ID: Pubkey = pubkey!("create1111111111111111111111111111111111111");

    pub fn process_instruction(_: &Pubkey, accounts: &[AccountInfo], _: &[u8]) -> ProgramResult {
        let [signer, new_account, ..] = accounts else {
            unreachable!();
        };

        let seeds = &[b"test".as_ref(), signer.key().as_ref()];
        let (_, bump) = find_program_address(seeds, &PROGRAM_ID);

        let lamports = Rent::get()?.minimum_balance(1000);

        CreateAccount {
            from: signer,
            to: new_account,
            lamports,
            space: 1000,
            owner: &PROGRAM_ID,
        }
        .invoke_signed(&[signer!(b"test", signer.key(), &[bump])])?;

        Ok(())
    }
}