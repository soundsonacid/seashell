# Seashell 🐚

Seashell is a lightweight, deterministic testing framework for Solana programs that enables reproducible testing against real mainnet data.

## Profiler

the seashell programs in spl/elfs ship with full dwarf symbols by default in their corresponding `.debug` files (this does not impact cu)

in order to use the profiler you must build your program unstripped, with debug info. what that takes depends on your platform-tools version:

**platform-tools >= v1.51**:

```bash
RUSTFLAGS="-C debuginfo=2 -C strip=none" cargo build-sbf --tools-version v1.54 --debug
```

**platform-tools <= v1.50**

```bash
PT=~/.cache/solana/v1.50/platform-tools
RUSTC_BOOTSTRAP=1 RUSTC=$PT/rust/bin/rustc \
RUSTFLAGS="-C debuginfo=2 -C strip=none -Z dwarf-version=5" \
    $PT/rust/bin/cargo build --release --target sbpf-solana-solana
```

### why those flags (sources)

- release builds generate no debug info by default (`debug = false`): [cargo book — profiles](https://doc.rust-lang.org/cargo/reference/profiles.html#release). hence `-C debuginfo=2` (or `cargo build-sbf --debug`, which appends `-g`: [cargo-build-sbf main.rs](https://github.com/anza-xyz/agave/blob/v3.0.10/platform-tools-sdk/cargo-build-sbf/src/main.rs#L202-L205))
- even with debug info generated, cargo passes `-C strip=debuginfo` for release profiles by default since rust 1.77, dropping the DWARF at link while keeping `.symtab`: [rust 1.77 announcement](https://blog.rust-lang.org/2024/03/21/Rust-1.77.0.html), [cargo book — strip](https://doc.rust-lang.org/cargo/reference/profiles.html#strip), [rustc `-C strip` semantics](https://doc.rust-lang.org/rustc/codegen-options/index.html#strip). `-C strip=none` in `RUSTFLAGS` wins because rustflags are appended after cargo's profile flags. verify: `cargo build -v` shows `-C strip=debuginfo` in the rustc invocation without the override
- the deploy `.so` is fully stripped — no `.symtab`, no DWARF: `cargo-build-sbf` post-processing runs `llvm-objcopy --strip-all` ([post_processing.rs](https://github.com/anza-xyz/agave/blob/v3.0.10/platform-tools-sdk/cargo-build-sbf/src/post_processing.rs#L70-L90) via [strip.sh](https://github.com/anza-xyz/agave/blob/v3.0.10/platform-tools-sdk/sbf/scripts/strip.sh); [`--strip-all` removes both](https://llvm.org/docs/CommandGuide/llvm-objcopy.html))
- the `.debug` companion is `llvm-objcopy --only-keep-debug` of the unstripped binary, emitted by `--debug`: [post_processing.rs](https://github.com/anza-xyz/agave/blob/v3.0.10/platform-tools-sdk/cargo-build-sbf/src/post_processing.rs#L122-L137)
- `-Z dwarf-version=5` on <= v1.50: the old sbpf lld applies `R_BPF_64_64` relocations to debug sections as if they were `lddw` instruction slots, corrupting dwarf <= 4 `.debug_info`; dwarf 5 keeps addresses in `.debug_addr` where the damage is recoverable. fixed in v1.51 — same rustc 1.84.1, only the linker changed ([anza-xyz/platform-tools releases](https://github.com/anza-xyz/platform-tools/releases)); provable byte-for-byte with the dwarf-demo in the profile-example repo

with process instruction:
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
```

with profile instruction:
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
    let result = seashell.profile_instruction(transfer_ix);

    // Verify the transfer succeeded
    assert!(result.error.is_none());

    // Check final balances
    assert_eq!(seashell.account(&alice).lamports(), alice_balance - 1_000_000);
    assert_eq!(seashell.account(&bob).lamports(), bob_balance + 1_000_000);

    // render svg to target/flamegraphs
    seashell.write_svg();
    // remove the profiler for subsequent runs
    seashell.clear_profiler();
}
```

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