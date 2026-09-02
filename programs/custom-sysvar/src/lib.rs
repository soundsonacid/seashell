#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use {
        pinocchio::{
            account_info::AccountInfo, entrypoint, program_error::ProgramError, pubkey::Pubkey,
            sysvars::get_sysvar, ProgramResult,
        },
    };

    const TEST_SYSVAR_ID: Pubkey = [42; 32];
    const EXPECTED_VALUE: u64 = 0x0123_4567_89ab_cdef;

    struct TestSysvar {
        value: u64,
    }

    impl TestSysvar {
        fn get() -> Result<Self, ProgramError> {
            let mut data = [0; size_of::<u64>()];
            get_sysvar(&mut data, &TEST_SYSVAR_ID, 0)?;
            Ok(Self {
                value: u64::from_le_bytes(data),
            })
        }
    }

    entrypoint!(process_instruction);

    pub fn process_instruction(_: &Pubkey, _: &[AccountInfo], _: &[u8]) -> ProgramResult {
        if TestSysvar::get()?.value != EXPECTED_VALUE {
            return Err(ProgramError::InvalidAccountData);
        }
        Ok(())
    }
}
