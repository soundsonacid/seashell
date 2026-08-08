#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use pinocchio::account_info::AccountInfo;
    use pinocchio::entrypoint;
    use pinocchio::program_error::ProgramError;
    use pinocchio::pubkey::Pubkey;
    use pinocchio::ProgramResult;

    entrypoint!(process_instruction);

    pub fn process_instruction(
        _program_id: &Pubkey,
        accounts: &[AccountInfo],
        instruction_data: &[u8],
    ) -> ProgramResult {
        let [target] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        let sum: u64 = instruction_data.iter().map(|b| *b as u64).sum();
        let mut data = target.try_borrow_mut_data()?;
        data.get_mut(..8)
            .ok_or(ProgramError::AccountDataTooSmall)?
            .copy_from_slice(&sum.to_le_bytes());
        Ok(())
    }
}
