#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use pinocchio::account_info::AccountInfo;
    use pinocchio::cpi::slice_invoke_signed;
    use pinocchio::entrypoint;
    use pinocchio::instruction::{AccountMeta, Instruction};
    use pinocchio::pubkey::Pubkey;
    use pinocchio::program_error::ProgramError;
    use pinocchio::ProgramResult;

    entrypoint!(process_instruction);

    pub fn process_instruction(
        _program_id: &Pubkey,
        accounts: &[AccountInfo],
        instruction_data: &[u8],
    ) -> ProgramResult {
        let [callee, forwarded @ ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        pinocchio_log::log!("cpi-caller: invoking callee");

        let metas: Vec<AccountMeta> = forwarded
            .iter()
            .map(|a| AccountMeta::new(a.key(), a.is_writable(), a.is_signer()))
            .collect();
        let instruction = Instruction {
            program_id: callee.key(),
            accounts: &metas,
            data: instruction_data,
        };
        let infos: Vec<&AccountInfo> = forwarded.iter().collect();
        slice_invoke_signed(&instruction, &infos, &[])?;

        pinocchio_log::log!("cpi-caller: callee returned");
        Ok(())
    }
}
