use solana_account::AccountSharedData;
use solana_clock::Clock;
use solana_epoch_rewards::EpochRewards;
use solana_epoch_schedule::EpochSchedule;
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_slot_hashes::{MAX_ENTRIES, SlotHashes};
use solana_stake_interface::stake_history::{StakeHistory, StakeHistoryEntry};
use solana_sysvar::last_restart_slot::LastRestartSlot;
use solana_sysvar_id::SysvarId;

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

    pub fn get(&self, sysvar: &Pubkey) -> AccountSharedData {
        match sysvar {
            _ if sysvar == &Clock::id() => AccountSharedData::new(0, 0, &Clock::id()),
            _ if sysvar == &EpochSchedule::id() => {
                AccountSharedData::new(0, 0, &EpochSchedule::id())
            }
            _ if sysvar == &EpochRewards::id() => AccountSharedData::new(0, 0, &EpochRewards::id()),
            _ if sysvar == &Rent::id() => AccountSharedData::new(0, 0, &Rent::id()),
            _ if sysvar == &SlotHashes::id() => AccountSharedData::new(0, 0, &SlotHashes::id()),
            _ if sysvar == &StakeHistory::id() => AccountSharedData::new(0, 0, &StakeHistory::id()),
            _ if sysvar == &LastRestartSlot::id() => {
                AccountSharedData::new(0, 0, &LastRestartSlot::id())
            }
            _ => panic!("Unknown sysvar: {sysvar}"),
        }
    }
}
