#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use pinocchio::account_info::AccountInfo;
    use pinocchio::pubkey::Pubkey;
    use pinocchio::entrypoint;
    use pinocchio::ProgramResult;

    entrypoint!(process_instruction);

    pub fn process_instruction(_: &Pubkey, accounts: &[AccountInfo], _: &[u8]) -> ProgramResult {
        for account in accounts {
            pinocchio::pubkey::log(account.key());
        }

        Ok(())
    }
}