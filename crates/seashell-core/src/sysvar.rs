use parking_lot::RwLock;
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
    clock: RwLock<Clock>,
    epoch_schedule: RwLock<EpochSchedule>,
    epoch_rewards: RwLock<EpochRewards>,
    rent: RwLock<Rent>,
    slot_hashes: RwLock<SlotHashes>,
    stake_history: RwLock<StakeHistory>,
    last_restart_slot: RwLock<LastRestartSlot>,
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
            clock: RwLock::new(clock),
            epoch_rewards: RwLock::new(epoch_rewards),
            epoch_schedule: RwLock::new(epoch_schedule),
            last_restart_slot: RwLock::new(last_restart_slot),
            rent: RwLock::new(rent),
            slot_hashes: RwLock::new(slot_hashes),
            stake_history: RwLock::new(stake_history),
        }
    }
}

impl Sysvars {
    pub fn clock(&self) -> Clock {
        self.clock.read().clone()
    }

    pub fn epoch_schedule(&self) -> EpochSchedule {
        self.epoch_schedule.read().clone()
    }

    pub fn epoch_rewards(&self) -> EpochRewards {
        self.epoch_rewards.read().clone()
    }

    pub fn rent(&self) -> Rent {
        self.rent.read().clone()
    }

    pub fn slot_hashes(&self) -> SlotHashes {
        SlotHashes::new(&self.slot_hashes.read())
    }

    pub fn stake_history(&self) -> StakeHistory {
        self.stake_history.read().clone()
    }

    pub fn last_restart_slot(&self) -> LastRestartSlot {
        self.last_restart_slot.read().clone()
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

    pub fn set(&self, sysvar: &Pubkey, account: AccountSharedData) {
        match sysvar {
            _ if sysvar == &Clock::id() => {
                *self.clock.write() = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &EpochSchedule::id() => {
                *self.epoch_schedule.write() = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &EpochRewards::id() => {
                *self.epoch_rewards.write() = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &Rent::id() => {
                *self.rent.write() = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &SlotHashes::id() => {
                *self.slot_hashes.write() = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &StakeHistory::id() => {
                *self.stake_history.write() = bincode::deserialize(account.data()).unwrap();
            }
            _ if sysvar == &LastRestartSlot::id() => {
                *self.last_restart_slot.write() = bincode::deserialize(account.data()).unwrap();
            }
            _ => panic!("Unknown sysvar: {sysvar}"),
        }
    }

    pub fn get(&self, sysvar: &Pubkey) -> AccountSharedData {
        match sysvar {
            _ if sysvar == &Clock::id() => {
                AccountSharedData::new_data(0, &*self.clock.read(), &SYSVAR).unwrap()
            }
            _ if sysvar == &EpochSchedule::id() => {
                AccountSharedData::new_data(0, &*self.epoch_schedule.read(), &SYSVAR).unwrap()
            }
            _ if sysvar == &EpochRewards::id() => AccountSharedData::new_data(
                0,
                &bincode::serialize(&*self.epoch_rewards.read()).unwrap(),
                &SYSVAR,
            )
            .unwrap(),
            _ if sysvar == &Rent::id() => {
                AccountSharedData::new_data(0, &*self.rent.read(), &SYSVAR).unwrap()
            }
            _ if sysvar == &SlotHashes::id() => {
                AccountSharedData::new_data(0, &*self.slot_hashes.read(), &SYSVAR).unwrap()
            }
            _ if sysvar == &StakeHistory::id() => {
                AccountSharedData::new_data(0, &*self.stake_history.read(), &SYSVAR).unwrap()
            }
            _ if sysvar == &LastRestartSlot::id() => {
                AccountSharedData::new_data(0, &*self.last_restart_slot.read(), &SYSVAR).unwrap()
            }
            _ => panic!("Unknown sysvar: {sysvar}"),
        }
    }

    pub fn warp(&self, slot: u64, timestamp: i64) {
        let mut clock = self.clock.write();
        clock.slot = slot;
        clock.unix_timestamp = timestamp;
    }
}