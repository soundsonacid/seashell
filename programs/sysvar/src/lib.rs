#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use pinocchio::account_info::AccountInfo;
    use pinocchio::pubkey::Pubkey;
    use pinocchio::entrypoint;
    use pinocchio::sysvars::Sysvar;
    use pinocchio::ProgramResult;

    entrypoint!(process_instruction);

    pub fn process_instruction(_: &Pubkey, _: &[AccountInfo], _: &[u8]) -> ProgramResult {
        let clock = pinocchio::sysvars::clock::Clock::get().unwrap();
        pinocchio_log::log!("Slot: {}", clock.slot);
        pinocchio_log::log!("Epoch: {}", clock.epoch);
        pinocchio_log::log!("Epoch Start Timestamp: {}", clock.epoch_start_timestamp);
        pinocchio_log::log!("Leader Schedule Epoch: {}", clock.leader_schedule_epoch);
        pinocchio_log::log!("Timestamp: {}", clock.unix_timestamp);

        Ok(())
    }
}