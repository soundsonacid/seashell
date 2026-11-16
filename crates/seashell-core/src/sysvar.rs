use std::collections::HashMap;

use parking_lot::RwLock;
use solana_account::{Account, AccountSharedData, ReadableAccount};
use solana_clock::Clock;
use solana_epoch_rewards::EpochRewards;
use solana_epoch_schedule::EpochSchedule;
use solana_hash::Hash;
use solana_instruction::{BorrowedAccountMeta, BorrowedInstruction, Instruction};
use solana_program_runtime::sysvar_cache::SysvarCache;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_slot_hashes::{SlotHash, SlotHashes, MAX_ENTRIES};
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
    pub custom_sysvars: RwLock<HashMap<Pubkey, Vec<u8>>>,
}

impl Default for Sysvars {
    fn default() -> Self {
        let clock = Clock::default();
        let epoch_rewards = EpochRewards::default();
        let epoch_schedule = EpochSchedule::without_warmup();
        let last_restart_slot = LastRestartSlot::default();
        let rent = Rent::default();

        let slot_hashes = {
            let mut default_slot_hashes = vec![SlotHash::new(0, Hash::default()); MAX_ENTRIES];
            default_slot_hashes[0] = SlotHash::new(clock.slot, Hash::default());
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
            custom_sysvars: RwLock::default(),
        }
    }
}

impl Sysvars {
    pub fn register_custom_sysvar<T: SysvarId + serde::Serialize>(&self, sysvar: &T) {
        let data = bincode::serialize(sysvar).expect("Failed to serialize custom sysvar");
        self.custom_sysvars.write().insert(T::id(), data);
    }

    pub fn deregister_custom_sysvar(&self, sysvar_id: &Pubkey) -> Option<Vec<u8>> {
        self.custom_sysvars.write().remove(sysvar_id)
    }

    pub fn populate_custom_sysvars(&self, sysvar_cache: &mut SysvarCache) {
        for (sysvar_id, data) in self.custom_sysvars.read().iter() {
            sysvar_cache.register_custom_sysvar_data(*sysvar_id, data.clone());
        }
    }

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
            _ if sysvar == &Clock::id() => *self.clock.write() = decode(&account),
            _ if sysvar == &EpochSchedule::id() => *self.epoch_schedule.write() = decode(&account),
            _ if sysvar == &EpochRewards::id() => *self.epoch_rewards.write() = decode(&account),
            _ if sysvar == &Rent::id() => *self.rent.write() = decode(&account),
            _ if sysvar == &SlotHashes::id() => *self.slot_hashes.write() = decode(&account),
            _ if sysvar == &StakeHistory::id() => *self.stake_history.write() = decode(&account),
            _ if sysvar == &LastRestartSlot::id() => {
                *self.last_restart_slot.write() = decode(&account)
            }
            _ => panic!("Unknown sysvar: {sysvar}"),
        }
    }

    pub fn get(&self, sysvar: &Pubkey) -> AccountSharedData {
        match sysvar {
            _ if sysvar == &Clock::id() => encode(&*self.clock.read()),
            _ if sysvar == &EpochSchedule::id() => encode(&*self.epoch_schedule.read()),
            _ if sysvar == &EpochRewards::id() => encode(&*self.epoch_rewards.read()),
            _ if sysvar == &Rent::id() => encode(&*self.rent.read()),
            _ if sysvar == &SlotHashes::id() => encode(&*self.slot_hashes.read()),
            _ if sysvar == &StakeHistory::id() => encode(&*self.stake_history.read()),
            _ if sysvar == &LastRestartSlot::id() => encode(&*self.last_restart_slot.read()),
            _ => panic!("Unknown sysvar: {sysvar}"),
        }
    }

    pub fn warp(&self, slot: u64, timestamp: i64) {
        let mut clock = self.clock.write();
        clock.slot = slot;
        clock.unix_timestamp = timestamp;
    }
}

/// The sysvar stored in the data of `account`.
fn decode<T: wincode::DeserializeOwned<Dst = T>>(account: &AccountSharedData) -> T {
    wincode::deserialize(account.data()).expect("Failed to deserialize sysvar")
}

/// The account holding `sysvar`, with the rent-exempt minimum of lamports as on a cluster.
fn encode<T: wincode::Serialize<Src = T>>(sysvar: &T) -> AccountSharedData {
    let data = wincode::serialize(sysvar).expect("Failed to serialize sysvar");
    let lamports = Rent::default().minimum_balance(data.len());
    AccountSharedData::new_with_data(lamports, data, &SYSVAR)
}

pub struct SysvarInstructions;

impl SysvarInstructions {
    /// The instructions sysvar for all the instructions of a transaction message.
    pub fn construct_instructions_account_for_message(
        message: &impl solana_svm_transaction::svm_message::SVMMessage,
    ) -> AccountSharedData {
        let account_keys = message.account_keys();
        let borrowed_instructions = message
            .program_instructions_iter()
            .map(|(program_id, instruction)| BorrowedInstruction {
                program_id,
                accounts: instruction
                    .accounts
                    .iter()
                    .map(|index| {
                        let index = usize::from(*index);
                        BorrowedAccountMeta {
                            pubkey: account_keys.get(index).expect("a sanitized message"),
                            is_signer: message.is_signer(index),
                            is_writable: message.is_writable(index),
                        }
                    })
                    .collect(),
                data: instruction.data,
            })
            .collect::<Vec<_>>();
        let data = solana_instructions_sysvar::construct_instructions_data(&borrowed_instructions)
            .expect("failed to construct instructions data");

        AccountSharedData::from(Account {
            data,
            owner: solana_sdk_ids::sysvar::id(),
            ..Account::default()
        })
    }

    pub fn construct_instructions_account(instruction: &Instruction) -> AccountSharedData {
        let borrowed_accounts = instruction.accounts.iter().map(|meta| {
            BorrowedAccountMeta {
                pubkey: &meta.pubkey,
                is_signer: meta.is_signer,
                is_writable: meta.is_writable,
            }
        }).collect::<Vec<_>>();

        let borrowed_instruction = BorrowedInstruction {
            program_id: &instruction.program_id,
            accounts: borrowed_accounts,
            data: &instruction.data,
        };

        let sysvar_instructions_data = solana_instructions_sysvar::construct_instructions_data(&[borrowed_instruction]).expect("failed to construct instructions data");

        AccountSharedData::from(Account {
            data: sysvar_instructions_data,
            owner: solana_sdk_ids::sysvar::id(),
            ..Account::default()
        })
    }
}