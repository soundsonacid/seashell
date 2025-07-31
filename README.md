# Seashell 🐚

Seashell is a lightweight, deterministic testing framework for Solana programs that enables reproducible testing against real mainnet data.

## Overview

Seashell provides a minimal SVM (Solana Virtual Machine) runtime that allows you to:
- Execute Solana programs in a controlled environment
- Test against real mainnet account state
- Create reproducible test scenarios
- Cache and replay mainnet data for deterministic testing

## Key Features

### Scenarios: Reproducible Mainnet Testing

The heart of Seashell is the **Scenario** system, which enables deterministic testing with real mainnet data. Here's how it works:

1. **Account Fetching**: When you configure an RPC URL, Seashell will automatically fetch any missing accounts from mainnet
2. **Automatic Persistence**: Fetched accounts are automatically saved to a compressed JSON file (`scenarios/*.json.gz`)
3. **Deterministic Replay**: On subsequent test runs, accounts are loaded from the scenario file instead of RPC, ensuring tests are fast and deterministic
4. **Version Control**: Scenario files can be committed to git, allowing your entire team to test against the same mainnet state

### How Scenarios Work

When you load a scenario, Seashell follows this account lookup order:
1. Check scenario overrides (accounts previously fetched from RPC)
2. Check manually set accounts
3. If not found and RPC is configured, fetch from mainnet and save to scenario
4. If RPC is not configured, panic with a helpful message

This design ensures that:
- You explicitly opt-in to RPC fetching by setting the `RPC_URL` environment variable
- Fetched accounts are automatically cached for future runs
- Tests remain deterministic once accounts are cached
- You can't accidentally fetch accounts without a scenario loaded

## Example Usage

```rust
use seashell::{Config, Seashell};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

#[test]
fn test_transfer_against_mainnet() {
    // Set RPC URL to enable account fetching (only needed on first run)
    std::env::set_var("RPC_URL", "https://api.mainnet-beta.solana.com");
    
    // Create a new Seashell instance
    let mut seashell = Seashell::new();
    
    // Load a scenario (creates scenarios/my_test.json.gz if it doesn't exist)
    seashell.load_scenario("my_test");
    
    // These accounts will be fetched from mainnet on first run,
    // then loaded from the scenario file on subsequent runs
    let alice = Pubkey::from_str("ALiCE...").unwrap();
    let bob = Pubkey::from_str("BoB...").unwrap();
    
    // Check initial balances (fetches from RPC if needed)
    let alice_balance = seashell.account(&alice).lamports();
    let bob_balance = seashell.account(&bob).lamports();
    
    // Create and execute a transfer instruction
    let transfer_ix = system_instruction::transfer(&alice, &bob, 1_000_000);
    let result = seashell.process_instruction(transfer_ix);
    
    // Verify the transfer succeeded
    assert!(result.error.is_none());
    
    // Check final balances
    assert_eq!(seashell.account(&alice).lamports(), alice_balance - 1_000_000);
    assert_eq!(seashell.account(&bob).lamports(), bob_balance + 1_000_000);
}

// On subsequent test runs, you can remove the RPC_URL since accounts are cached:
#[test]
fn test_cached_scenario() {
    // No RPC_URL needed - accounts will be loaded from scenarios/my_test.json.gz
    let mut seashell = Seashell::new();
    seashell.load_scenario("my_test");
    
    // Same test code works with cached data
    let alice = Pubkey::from_str("ALiCE...").unwrap();
    assert!(seashell.account(&alice).lamports() > 0);
}
```

## Best Practices

1. **First Run**: Set `RPC_URL` environment variable to fetch accounts from mainnet
2. **Subsequent Runs**: Remove `RPC_URL` to use cached scenario data
3. **Version Control**: Commit your `scenarios/*.json.gz` files to ensure reproducible tests across your team
4. **Scenario Names**: Use descriptive scenario names that indicate what they're testing
5. **Account Management**: Let Seashell manage account fetching - don't manually set accounts that should come from mainnet

## Environment Variables

- `RPC_URL`: Solana RPC endpoint URL (e.g., `https://api.mainnet-beta.solana.com`). When set, enables automatic account fetching for missing accounts.