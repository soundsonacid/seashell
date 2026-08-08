#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use pinocchio::account_info::AccountInfo;
    use pinocchio::cpi::invoke_signed;
    use pinocchio::entrypoint;
    use pinocchio::instruction::{AccountMeta, Instruction};
    use pinocchio::program_error::ProgramError;
    use pinocchio::pubkey::Pubkey;
    use pinocchio::ProgramResult;

    entrypoint!(process_instruction);

    pub fn process_instruction(
        _program_id: &Pubkey,
        accounts: &[AccountInfo],
        instruction_data: &[u8],
    ) -> ProgramResult {
        let [source, destination, authority, token_program] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        let amount = u64::from_le_bytes(
            instruction_data
                .get(..8)
                .ok_or(ProgramError::InvalidInstructionData)?
                .try_into()
                .unwrap(),
        );

        let mut data = [0u8; 9];
        data[0] = 3;
        data[1..9].copy_from_slice(&amount.to_le_bytes());
        let metas = [
            AccountMeta::new(source.key(), true, false),
            AccountMeta::new(destination.key(), true, false),
            AccountMeta::new(authority.key(), false, true),
        ];
        let instruction = Instruction {
            program_id: token_program.key(),
            accounts: &metas,
            data: &data,
        };
        invoke_signed(&instruction, &[source, destination, authority], &[])?;
        Ok(())
    }
}
