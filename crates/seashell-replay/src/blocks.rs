//! Blocks as `getBlock` returns them (`encoding: base64`, `transactionDetails: full`), one JSON
//! response per file, named `<slot>.json`.

use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;

use agave_reserved_account_keys::ReservedAccountKeys;
use base64::Engine;
use serde_json::Value;
use solana_hash::Hash;
use solana_message::v0::LoadedAddresses;
use seashell::{Seashell, TransactionLimits};

use crate::state::Start;
use solana_message::{SanitizedMessage, SimpleAddressLoader};
use solana_pubkey::Pubkey;
use solana_transaction::sanitized::SanitizedTransaction;
use solana_transaction::versioned::sanitized::SanitizedVersionedTransaction;
use solana_transaction::versioned::VersionedTransaction;

pub struct Block {
    pub slot: u64,
    pub block_time: i64,
    /// The hash of the block before this one: the latest blockhash while this block executes.
    pub previous_blockhash: Hash,
    pub transactions: Vec<BlockTransaction>,
}

pub struct BlockTransaction {
    pub transaction: SanitizedTransaction,
    /// What the cluster recorded for this transaction.
    pub fee: u64,
    pub succeeded: bool,
    pub compute_units_consumed: Option<u64>,
    pub log_messages: Vec<String>,
    pub signature: String,
    /// Lamports of every account of the message, in message order, before and after.
    pub pre_balances: Vec<u64>,
    pub post_balances: Vec<u64>,
    /// Token amounts of the message's token accounts, by index into the message's accounts,
    /// before and after.
    pub pre_token_amounts: Vec<(usize, u64)>,
    pub post_token_amounts: Vec<(usize, u64)>,
}

/// The blocks in `directory`, in slot order.
pub fn read_blocks(directory: &Path) -> Vec<Block> {
    let reserved_account_keys = ReservedAccountKeys::new_all_activated().active;
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
        .collect();
    paths.sort();

    paths
        .iter()
        .map(|path| {
            let slot = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse().ok())
                .unwrap_or_else(|| panic!("{} is not named <slot>.json", path.display()));
            let response: Value = serde_json::from_slice(&std::fs::read(path).unwrap())
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let block = &response["result"];
            let transactions = block["transactions"]
                .as_array()
                .unwrap_or_else(|| panic!("{} holds no block", path.display()))
                .iter()
                .map(|transaction| block_transaction(transaction, &reserved_account_keys))
                .collect();
            let previous_blockhash = block["previousBlockhash"]
                .as_str()
                .and_then(|hash| Hash::from_str(hash).ok())
                .unwrap_or_else(|| panic!("{} has no previous blockhash", path.display()));
            Block {
                slot,
                block_time: block["blockTime"].as_i64().unwrap_or_default(),
                previous_blockhash,
                transactions,
            }
        })
        .collect()
}

fn block_transaction(json: &Value, reserved_account_keys: &HashSet<Pubkey>) -> BlockTransaction {
    let meta = &json["meta"];
    let wire = base64::engine::general_purpose::STANDARD
        .decode(json["transaction"][0].as_str().expect("a base64 transaction"))
        .expect("valid base64");
    let transaction: VersionedTransaction =
        wincode::deserialize(&wire).expect("a transaction in wire format");

    let addresses = |kind: &str| -> Vec<Pubkey> {
        meta["loadedAddresses"][kind]
            .as_array()
            .map(|addresses| {
                addresses
                    .iter()
                    .map(|address| Pubkey::from_str(address.as_str().unwrap()).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    };
    // The cluster already resolved the lookup tables. Its answer is in the metadata.
    let loaded_addresses =
        LoadedAddresses { writable: addresses("writable"), readonly: addresses("readonly") };
    // Nothing in the replay looks at the message hash.
    let transaction = SanitizedTransaction::try_new(
        SanitizedVersionedTransaction::try_from(transaction)
            .expect("a transaction that landed in a block sanitizes"),
        Hash::default(),
        false,
        SimpleAddressLoader::Enabled(loaded_addresses),
        reserved_account_keys,
    )
    .expect("a transaction that landed in a block sanitizes");

    let log_messages = meta["logMessages"]
        .as_array()
        .map(|lines| lines.iter().map(|line| line.as_str().unwrap_or_default().to_string()).collect())
        .unwrap_or_default();
    let balances = |field: &str| -> Vec<u64> {
        meta[field]
            .as_array()
            .map(|balances| balances.iter().map(|balance| balance.as_u64().unwrap_or(0)).collect())
            .unwrap_or_default()
    };
    let token_amounts = |field: &str| -> Vec<(usize, u64)> {
        meta[field]
            .as_array()
            .map(|balances| {
                balances
                    .iter()
                    .filter_map(|balance| {
                        let index = balance["accountIndex"].as_u64()? as usize;
                        let amount = balance["uiTokenAmount"]["amount"].as_str()?.parse().ok()?;
                        Some((index, amount))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    BlockTransaction {
        pre_balances: balances("preBalances"),
        post_balances: balances("postBalances"),
        pre_token_amounts: token_amounts("preTokenBalances"),
        post_token_amounts: token_amounts("postTokenBalances"),
        signature: transaction.signature().to_string(),
        log_messages,
        transaction,
        fee: meta["fee"].as_u64().unwrap_or_default(),
        succeeded: meta["err"].is_null(),
        compute_units_consumed: meta["computeUnitsConsumed"].as_u64(),
    }
}

/// Sets the clock and the latest blockhash to what the transactions of `blocks[index]` see.
///
/// The slot is the block's. The timestamp is the one the block before it recorded, because the
/// cluster updates the clock's timestamp when a block ends: `blockTime` is what the next block
/// sees. The first block sees the snapshot's.
pub fn enter_block(seashell: &mut Seashell, blocks: &[Block], index: usize, start: &Start) {
    let block = &blocks[index];
    let timestamp = match index.checked_sub(1) {
        Some(previous) => blocks[previous].block_time,
        None => start.timestamp,
    };
    seashell.accounts_db.warp(block.slot, timestamp);
    seashell.blockhash = block.previous_blockhash;
}

/// The compute unit limit and heap size a transaction asks for: in the config of a v1 message,
/// with compute budget instructions in the older formats.
pub fn limits(seashell: &Seashell, transaction: &SanitizedTransaction) -> TransactionLimits {
    /// The heap a v1 transaction gets when its config does not name a size.
    const DEFAULT_HEAP_SIZE: u32 = 32 * 1024;
    match transaction.message() {
        SanitizedMessage::V1(message) => {
            let config = &message.message.config;
            TransactionLimits {
                compute_unit_limit: config.compute_unit_limit.unwrap_or(0),
                heap_size: config.heap_size.unwrap_or(DEFAULT_HEAP_SIZE),
            }
        }
        _ => seashell.limits_from_compute_budget_instructions(transaction),
    }
}

/// Every address the transactions of `blocks` name, lookup table addresses included.
pub fn addresses(blocks: &[Block]) -> HashSet<Pubkey> {
    blocks
        .iter()
        .flat_map(|block| &block.transactions)
        .flat_map(|transaction| transaction.transaction.message().account_keys().iter().copied())
        .collect()
}
