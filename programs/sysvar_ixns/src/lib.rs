#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use pinocchio::account_info::AccountInfo;
    use pinocchio::pubkey::Pubkey;
    use pinocchio::entrypoint;
    use pinocchio::ProgramResult;

    entrypoint!(process_instruction);

    pub fn process_instruction(_: &Pubkey, accounts: &[AccountInfo], _: &[u8]) -> ProgramResult {
        let sysvar_ixns = unsafe { accounts[0].borrow_data_unchecked() };
        let sysvar = unsafe { pinocchio::sysvars::instructions::Instructions::new_unchecked(sysvar_ixns) };
        let ixn = sysvar.get_instruction_relative(0)?;
        let pubkey = ixn.get_program_id();
        pinocchio::pubkey::log(pubkey);
        Ok(())
    }
}
