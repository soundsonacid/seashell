use solana_account::{AccountSharedData, ReadableAccount};
use solana_clock::Clock;
use solana_epoch_rewards::EpochRewards;
use solana_epoch_schedule::EpochSchedule;
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_slot_hashes::{SlotHashes, MAX_ENTRIES};
use solana_stake_interface::stake_history::{StakeHistory, StakeHistoryEntry};
use solana_sysvar::last_restart_slot::LastRestartSlot;
use solana_sysvar_id::{SysvarId, ID as SYSVAR};

pub struct Sysvars {
    pub clock: Clock,
    pub epoch_schedule: EpochSchedule,
    pub epoch_rewards: EpochRewards,
    pub rent: Rent,
    pub slot_hashes: SlotHashes,
    pub stake_history: StakeHistory,
    pub last_restart_slot: LastRestartSlot,
}

impl Default for Sysvars {
    fn default() -> Self {
        let clock = Clock::default();
        let epoch_rewards = EpochRewards::default();
        let epoch_schedule = EpochSchedule::without_warmup();
        let last_restart_slot = LastRestartSlot::default();
        let rent = Rent::default();

        let slot_hashes = {
            let mut default_slot_hashes = vec![(0, Hash::default()); MAX_ENTRIES];
            default_slot_hashes[0] = (clock.slot, Hash::default());
            SlotHashes::new(&default_slot_hashes)
        };

        let mut stake_history = StakeHistory::default();
        stake_history.add(clock.epoch, StakeHistoryEntry::default());

        Self {
            clock,
            epoch_rewards,
            epoch_schedule,
            last_restart_slot,
            rent,
            slot_hashes,
            stake_history,
        }
    }
}

impl Sysvars {
    pub fn clock(&self) -> Clock {
        self.clock.clone()
    }

    pub fn epoch_schedule(&self) -> EpochSchedule {
        self.epoch_schedule.clone()
    }

    pub fn epoch_rewards(&self) -> EpochRewards {
        self.epoch_rewards.clone()
    }

    pub fn rent(&self) -> Rent {
        self.rent.clone()
    }

    pub fn slot_hashes(&self) -> SlotHashes {
        SlotHashes::new(&self.slot_hashes)
    }

    pub fn stake_history(&self) -> StakeHistory {
        self.stake_history.clone()
    }

    pub fn last_restart_slot(&self) -> LastRestartSlot {
        self.last_restart_slot.clone()
    }

    pub fn is_sysvar(&self, sysvar: &Pubkey) -> bool {
        sysvar == &Clock::id()
            || sysvar == &EpochSchedule::id()
            || sysvar == &EpochRewards::id()
            || sysvar == &Rent::id()
            || sysvar == &SlotHashes::id()
            || sysvar == &StakeHistory::id()
            || sysvar == &LastRestartSlot::id()
    }

    pub fn set(&mut self, sysvar: &Pubkey, account: AccountSharedData) {
        match sysvar {
            _ if sysvar == &Clock::id() => {
                self.clock = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &EpochSchedule::id() => {
                self.epoch_schedule = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &EpochRewards::id() => {
                self.epoch_rewards = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &Rent::id() => {
                self.rent = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &SlotHashes::id() => {
                self.slot_hashes = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &StakeHistory::id() => {
                self.stake_history = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &LastRestartSlot::id() => {
                self.last_restart_slot = bincode::deserialize(account.data()).unwrap();
            }
            _ => panic!("Unknown sysvar: {sysvar}"),
        }
    }

    pub fn get(&self, sysvar: &Pubkey) -> AccountSharedData {
        match sysvar {
            _ if sysvar == &Clock::id() => {
                AccountSharedData::new_data(0, &self.clock, &SYSVAR).unwrap()
            }
            _ if sysvar == &EpochSchedule::id() => {
                AccountSharedData::new_data(0, &self.epoch_schedule, &SYSVAR).unwrap()
            }
            _ if sysvar == &EpochRewards::id() => AccountSharedData::new_data(
                0,
                &bincode::serialize(&self.epoch_rewards).unwrap(),
                &SYSVAR,
            )
            .unwrap(),
            _ if sysvar == &Rent::id() => {
                AccountSharedData::new_data(0, &self.rent, &SYSVAR).unwrap()
            }
            _ if sysvar == &SlotHashes::id() => {
                AccountSharedData::new_data(0, &self.slot_hashes, &SYSVAR).unwrap()
            }
            _ if sysvar == &StakeHistory::id() => {
                AccountSharedData::new_data(0, &self.stake_history, &SYSVAR).unwrap()
            }
            _ if sysvar == &LastRestartSlot::id() => {
                AccountSharedData::new_data(0, &self.last_restart_slot, &SYSVAR).unwrap()
            }
            _ => panic!("Unknown sysvar: {sysvar}"),
        }
    }
}
