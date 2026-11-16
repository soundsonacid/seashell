//! Why the replay differs from the cluster: is a transaction that does not match given the
//! wrong state to start from, or does it execute differently from the same state?
//!
//! The cluster records every account's lamports and every token account's amount before and
//! after each transaction. So before the replay runs a transaction, its state can be checked
//! against the cluster's, and after it, its effect. That separates three things:
//!
//! - the state was already wrong, and where the wrong account came from
//! - the state the cluster shows was right, and the execution still differs
//! - the execution matches, and the state it leaves is wrong anyway

use std::collections::HashMap;

use seashell::{ExecutionMode, Seashell};
use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_pubkey::Pubkey;
use solana_svm_log_collector::LogCollector;

use crate::blocks::{Block, BlockTransaction};
use crate::fidelity::{divergence, Divergence};
use crate::state::Start;

/// What last happened to an account in the replay.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum LastWrite {
    /// Nothing: it is as the snapshot had it.
    Snapshot,
    /// A transaction that matched the cluster wrote it.
    MatchingTransaction(usize),
    /// A transaction that did not match the cluster wrote it, or should have.
    MismatchingTransaction(usize),
}

/// An account whose replay state differs from what the cluster recorded.
struct WrongAccount {
    address: Pubkey,
    what: &'static str,
    cluster: u64,
    replay: u64,
}

/// The token amount in a token account's data.
fn token_amount(account: &AccountSharedData) -> Option<u64> {
    let owner = account.owner();
    if *owner != spl_token_id() && *owner != spl_token_2022_id() {
        return None;
    }
    account.data().get(64..72).map(|amount| u64::from_le_bytes(amount.try_into().unwrap()))
}

fn spl_token_id() -> Pubkey {
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".parse().unwrap()
}

fn spl_token_2022_id() -> Pubkey {
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb".parse().unwrap()
}

/// The first account of the transaction whose lamports or token amount in the replay is not
/// what the cluster recorded.
fn first_wrong_account(
    seashell: &Seashell,
    keys: &[Pubkey],
    balances: &[u64],
    token_amounts: &[(usize, u64)],
) -> Option<WrongAccount> {
    for (address, &cluster) in keys.iter().zip(balances) {
        let replay = seashell.accounts_db.account_maybe(address).map_or(0, |a| a.lamports());
        if replay != cluster {
            return Some(WrongAccount { address: *address, what: "lamports", cluster, replay });
        }
    }
    for &(index, cluster) in token_amounts {
        let address = keys[index];
        let replay = seashell
            .accounts_db
            .account_maybe(&address)
            .and_then(|account| token_amount(&account))
            .unwrap_or(0);
        if replay != cluster {
            return Some(WrongAccount { address, what: "token amount", cluster, replay });
        }
    }
    None
}

fn short(address: &Pubkey) -> String {
    address.to_string()
}

pub fn report(
    seashell: &mut Seashell,
    start: &Start,
    blocks: &[Block],
) {
    start.restore_accounts(seashell);
    let initial_accounts = &start.accounts;
    seashell.execution_mode = ExecutionMode::Jit;

    let mut last_write: HashMap<Pubkey, LastWrite> = HashMap::new();
    let mut signatures: Vec<&str> = Vec::new();

    // Counters. "wrong input" means the lamports or token amounts the transaction starts from
    // differ from the cluster's.
    let mut total = 0;
    let mut matching_with_right_input = 0;
    let mut matching_with_wrong_input = 0;
    let mut mismatching_with_wrong_input = 0;
    let mut mismatching_with_right_input: Vec<(u64, usize, &BlockTransaction, Divergence, String)> =
        Vec::new();
    // Matching execution from the right input that leaves the wrong state.
    let mut wrong_output: Vec<(u64, usize, &BlockTransaction, WrongAccount)> = Vec::new();
    // Per wrong input account: where it came from, how many transactions it is the first wrong
    // account of, and the first of them.
    let mut wrong_inputs: HashMap<Pubkey, (LastWrite, &'static str, u64, u64, usize, usize)> =
        HashMap::new();
    let mut wrong_input_origins: HashMap<&'static str, usize> = HashMap::new();

    for (block_index, block) in blocks.iter().enumerate() {
        crate::blocks::enter_block(seashell, blocks, block_index, start);
        for transaction in &block.transactions {
            let index = total;
            total += 1;
            signatures.push(&transaction.signature);
            let message = transaction.transaction.message();
            let keys: Vec<Pubkey> = message.account_keys().iter().copied().collect();

            let wrong_input = first_wrong_account(
                seashell,
                &keys,
                &transaction.pre_balances,
                &transaction.pre_token_amounts,
            );

            let fee_payer = keys[0];
            let mut account = seashell.accounts_db.account_maybe(&fee_payer).unwrap_or_default();
            account.set_lamports(account.lamports().saturating_sub(transaction.fee));
            seashell.accounts_db.set_account(fee_payer, account);

            let log_collector = LogCollector::new_ref_with_limit(None);
            seashell.log_collector = Some(log_collector.clone());
            let limits = crate::blocks::limits(seashell, &transaction.transaction);
            let result = seashell.process_transaction(&transaction.transaction, limits);
            seashell.log_collector = None;
            let logs = log_collector.borrow().get_recorded_content().to_vec();

            let matches = result.error.is_none() == transaction.succeeded
                && transaction.compute_units_consumed == Some(result.compute_units_consumed);
            let write = if matches {
                LastWrite::MatchingTransaction(index)
            } else {
                LastWrite::MismatchingTransaction(index)
            };

            match (&wrong_input, matches) {
                (None, true) => {
                    matching_with_right_input += 1;
                    if let Some(wrong) = first_wrong_account(
                        seashell,
                        &keys,
                        &transaction.post_balances,
                        &transaction.post_token_amounts,
                    ) {
                        wrong_output.push((block.slot, index, transaction, wrong));
                    }
                }
                (None, false) => {
                    let why = format!("{:?}", result.error.as_ref().map(|(_, error)| error));
                    mismatching_with_right_input.push((
                        block.slot,
                        index,
                        transaction,
                        divergence(&transaction.log_messages, &logs),
                        why,
                    ));
                }
                (Some(wrong), _) => {
                    if matches {
                        matching_with_wrong_input += 1;
                    } else {
                        mismatching_with_wrong_input += 1;
                    }
                    let origin =
                        last_write.get(&wrong.address).copied().unwrap_or(LastWrite::Snapshot);
                    let entry = wrong_inputs.entry(wrong.address).or_insert((
                        origin,
                        wrong.what,
                        wrong.cluster,
                        wrong.replay,
                        0,
                        index,
                    ));
                    entry.4 += 1;
                    *wrong_input_origins
                        .entry(match origin {
                            LastWrite::Snapshot => "never written in the replay (snapshot state)",
                            LastWrite::MatchingTransaction(_) => {
                                "last written by a transaction that matched"
                            }
                            LastWrite::MismatchingTransaction(_) => {
                                "last written by a transaction that did not match"
                            }
                        })
                        .or_default() += 1;
                }
            }

            // Every writable account of the transaction is now what this transaction made it,
            // or failed to make it.
            for (position, address) in keys.iter().enumerate() {
                if message.is_writable(position) {
                    last_write.insert(*address, write);
                }
            }
        }
    }

    println!("{total} transactions, replayed once with the legacy jit");
    println!();
    println!("| state the transaction starts from | execution matches | execution differs |");
    println!("|---|---:|---:|");
    println!(
        "| lamports and token amounts as on the cluster | {matching_with_right_input} | {} |",
        mismatching_with_right_input.len()
    );
    println!(
        "| some lamports or token amount already wrong | {matching_with_wrong_input} | \
         {mismatching_with_wrong_input} |"
    );
    println!();
    println!(
        "of the {matching_with_right_input} that start right and execute the same, {} leave a \
         wrong lamport or token amount behind",
        wrong_output.len()
    );

    println!();
    println!("where the first wrong input account of a transaction came from");
    println!();
    println!("| origin | transactions |");
    println!("|---|---:|");
    let mut origins: Vec<_> = wrong_input_origins.into_iter().collect();
    origins.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (origin, count) in origins {
        println!("| {origin} | {count} |");
    }

    println!();
    println!("wrong input accounts, by how many transactions they are the first wrong account of (top 25)");
    println!();
    println!("| account | owner | transactions | what | cluster | replay | origin | first seen in |");
    println!("|---|---|---:|---|---:|---:|---|---|");
    let mut accounts: Vec<_> = wrong_inputs.into_iter().collect();
    accounts.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.4));
    for (address, (origin, what, cluster, replay, count, first)) in accounts.iter().take(25) {
        let owner = initial_accounts
            .get(address)
            .map_or("(not in snapshot)".to_string(), |account| short(account.owner()));
        let origin = match origin {
            LastWrite::Snapshot => "snapshot".to_string(),
            LastWrite::MatchingTransaction(index) => format!("matching tx {}", &signatures[*index][..12]),
            LastWrite::MismatchingTransaction(index) => {
                format!("mismatching tx {}", &signatures[*index][..12])
            }
        };
        println!(
            "| {} | {owner} | {count} | {what} | {cluster} | {replay} | {origin} | {} |",
            short(address),
            &signatures[*first][..12]
        );
    }

    println!();
    println!(
        "the {} transactions that start from the right lamports and token amounts and execute \
         differently (first 40, in block order)",
        mismatching_with_right_input.len()
    );
    for (slot, index, transaction, divergence, why) in mismatching_with_right_input.iter().take(40) {
        println!();
        println!(
            "slot {slot} tx {index} {}: {:?} in {}, CU cluster {:?}, replay ends with {why}",
            transaction.signature,
            divergence.kind,
            divergence.program,
            transaction.compute_units_consumed,
        );
        println!("  cluster: {}", divergence.cluster_line);
        println!("  replay:  {}", divergence.replay_line);
        let sysvars: Vec<String> = transaction
            .transaction
            .message()
            .account_keys()
            .iter()
            .map(|key| key.to_string())
            .filter(|key| key.starts_with("Sysvar"))
            .collect();
        println!("  sysvar accounts it names: {sysvars:?}");
    }
    let mut by_program: HashMap<(String, String), usize> = HashMap::new();
    for (_, _, _, divergence, _) in &mismatching_with_right_input {
        *by_program
            .entry((divergence.program.clone(), format!("{:?}", divergence.kind)))
            .or_default() += 1;
    }
    let mut by_program: Vec<_> = by_program.into_iter().collect();
    by_program.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    println!();
    println!("the same, by program and kind (top 30)");
    println!();
    println!("| program | kind | transactions |");
    println!("|---|---|---:|");
    for ((program, kind), count) in by_program.iter().take(30) {
        println!("| {program} | {kind} | {count} |");
    }

    println!();
    println!("matching executions from the right input that leave wrong state (first 25)");
    println!();
    println!("| slot | transaction | account | owner | what | cluster | replay |");
    println!("|---|---|---|---|---|---:|---:|");
    for (slot, _, transaction, wrong) in wrong_output.iter().take(25) {
        let owner = seashell
            .accounts_db
            .account_maybe(&wrong.address)
            .map_or("(absent)".to_string(), |account| short(account.owner()));
        println!(
            "| {slot} | {} | {} | {owner} | {} | {} | {} |",
            &transaction.signature[..12],
            short(&wrong.address),
            wrong.what,
            wrong.cluster,
            wrong.replay
        );
    }
    let mut output_accounts: HashMap<Pubkey, usize> = HashMap::new();
    for (_, _, _, wrong) in &wrong_output {
        *output_accounts.entry(wrong.address).or_default() += 1;
    }
    let mut output_accounts: Vec<_> = output_accounts.into_iter().collect();
    output_accounts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!();
    println!("accounts most often left wrong by such transactions (top 10)");
    for (address, count) in output_accounts.iter().take(10) {
        println!("  {address}: {count}");
    }
}
