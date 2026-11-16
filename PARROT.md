# Benchmarking parrot against the legacy SBPF engines

This branch (`frank/parrot`) runs SBF programs in four execution modes and compares them.

| mode | what runs the program |
|---|---|
| legacy interp | the interpreter in `solana-sbpf` |
| legacy jit | the JIT compiler in `solana-sbpf` |
| parrot interp | the threaded interpreter of anza-xyz/sbpf `parrot/perf` |
| parrot jit | the copy-and-patch JIT of the same branch |

There are two benchmarks. The falcon bench runs one compute-heavy program and needs nothing
beyond the repositories. The mainnet bench replays real blocks on account state taken from a
snapshot, and needs a snapshot and an RPC endpoint.

## 1. Check out the repositories

Two checkouts are needed, next to each other.

```text
some-dir/
  seashell/     github.com/soundsonacid/seashell      branch frank/parrot
  firedancer/   github.com/firedancer-io/firedancer   main
```

```sh
git clone -b frank/parrot git@github.com:soundsonacid/seashell
git clone git@github.com:firedancer-io/firedancer
```

Firedancer is only read as source. The crate `seashell-snapshot` compiles Firedancer's
streaming snapshot parser (`src/discof/restore/utils/fd_ssparse.c`) straight out of the
checkout. Firedancer itself does not have to be built. Set `FIREDANCER_DIR` if the checkout is
somewhere else. Firedancer was at `7813ec5ced` when this was written.

Cargo fetches the other two repositories itself. They are git dependencies pinned by revision
in `Cargo.toml`.

- `github.com/soundsonacid/sbpf`, branch `frank/parrot`. It is `parrot/perf` plus the seashell
  profiler, a `ParrotJit` execution mode, the execution timer (`src/timing.rs`, feature
  `execution-timing`) and the call dispatch counters (`src/codegen/call_dispatch.rs`,
  experiment code).
- `github.com/soundsonacid/agave`, branch `frank/parrot`. It is a recent master that speaks
  that sbpf, with the execution mode selectable per `InvokeContext`. It names the sbpf
  revision it builds against, and seashell has to name the same one.

To work on either of them, clone it and point seashell at the clone with a `[patch]` section,
or change the revisions in `Cargo.toml` after pushing.

## 2. Requirements

- x86_64 Linux. Parrot has no other backend on this branch.
- The Rust toolchain in `rust-toolchain.toml` (1.99.1). `rustup` installs it on first build.
- A C compiler (`cc`), for the snapshot parser and for zstd.
- For the mainnet bench, about 140 GB of disk for the snapshot and a mainnet RPC endpoint that
  serves `getBlock`.

## 3. The falcon bench

Falcon-512 signature verification, from blueshift-gg/solana-falcon512 at `0ee114f`, built as
SBPF v3. The program and a sample signature are checked in under
`crates/seashell-core/tests/fixtures/falcon512-blueshift/`.

```sh
cd seashell
cargo test --release -p seashell --test falcon_execution_modes --no-run
```

Cargo prints the path of the test binary. Run it pinned to one core.

```sh
RUST_LOG=error taskset -c 12 target/release/deps/falcon_execution_modes-<hash> \
    --ignored --nocapture --test-threads 1
```

It prints two tables. The first is the execution time of the program in each mode, over 2,000
executions per mode. The second is how long each JIT takes to compile the program.

The time is agave's own timer around the program's run in the VM, in whole microseconds. It
leaves out account loading, serialization, VM creation and compilation. It includes syscalls,
of which this program makes very few.

`RUST_LOG=error` matters. Seashell turns on debug logging of the VM by default, which is slow.

## 4. The mainnet bench

The bench needs three things on disk.

1. A full snapshot and an incremental snapshot on top of it.
2. The blocks that follow the incremental snapshot's slot.
3. The accounts those blocks name, extracted from the snapshots.

The examples below keep everything under one directory.

```sh
export DATA=/mnt/nvme2/replay
mkdir -p $DATA/snapshots $DATA/blocks
```

### 4.1 Download a snapshot

Any full and incremental pair from the same node works. The run this was written from used
[solana-snapshot-finder](https://github.com/c29r3/solana-snapshot-finder), which picks a fast
RPC node and downloads both files. Afterwards the directory holds two archives.

```text
snapshot-<full slot>-<hash>.tar.zst                               about 135 GB
incremental-snapshot-<full slot>-<snapshot slot>-<hash>.tar.zst   about 300 MB
```

The second number in the incremental file's name is the snapshot slot. The replay starts at
the slot after it.

### 4.2 Fetch the blocks

Fetch the blocks soon after the snapshot, or use an endpoint with history. `RPC_URL` is the
endpoint. Keep it out of files if it carries a token.

```sh
export RPC_URL=<mainnet rpc endpoint>
SNAPSHOT_SLOT=<snapshot slot>
BLOCKS=100

got=0
slot=$((SNAPSHOT_SLOT + 1))
while [ $got -lt $BLOCKS ]; do
  curl -s --max-time 120 -H 'content-type: application/json' "$RPC_URL" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getBlock\",\"params\":[$slot,{\"encoding\":\"base64\",\"maxSupportedTransactionVersion\":1,\"transactionDetails\":\"full\",\"rewards\":false}]}" \
    -o $DATA/blocks/$slot.json
  if grep -q '"blockhash"' $DATA/blocks/$slot.json; then
    got=$((got + 1))
  else
    rm -f $DATA/blocks/$slot.json   # a skipped slot has no block
  fi
  slot=$((slot + 1))
done
```

Three of those parameters are required.

- `encoding: base64` and `transactionDetails: full`, because the replay decodes the wire
  transactions and reads their metadata.
- `maxSupportedTransactionVersion: 1`. About half of mainnet's transactions are version 1, and
  a lower value makes the request fail.

Start at the slot right after the snapshot slot and do not leave gaps, other than skipped
slots. Each block's transactions see the clock of the block before, and the first block sees
the snapshot's.

### 4.3 Extract the accounts

```sh
cargo build --release -p seashell-replay

target/release/seashell-replay extract $DATA/blocks $DATA/accounts.bin \
    $DATA/snapshots/snapshot-<full slot>-<hash>.tar.zst \
    $DATA/snapshots/incremental-snapshot-<full slot>-<snapshot slot>-<hash>.tar.zst
```

The full snapshot goes first and the incremental one second. This reads both archives once and
keeps the accounts the blocks name, the program data of every program among them, the feature
accounts and the sysvars. Nothing is unpacked to disk. A 135 GB full snapshot (495 GiB
uncompressed, 1.2 billion account entries) took about two and a half minutes on one thread.

Run it again whenever the set of blocks changes.

### 4.4 Run the bench

```sh
RUST_LOG=error taskset -c 12 target/release/seashell-replay bench \
    $DATA/blocks $DATA/accounts.bin 5
```

The last argument is the number of measured rounds. 100 slots with 5 rounds take about seven
minutes. Pin it to a core that is otherwise idle.

What happens in a round. For each of the four modes the state is put back to the snapshot,
every transaction is replayed once without measuring, the state is put back again, and every
transaction is replayed once more with the clocks on. Round 0 is thrown away.

What it prints, in order.

| section | what it is |
|---|---|
| replay against the cluster | how many transactions end the way they did on mainnet, with the same compute units |
| the modes against each other | transactions whose outcome, compute units or syscall count differ between modes. This should be 0 for every mode |
| total program execution | instruction time plus syscall time, p50 and average over the rounds |
| instructions only | time executing BPF instructions |
| syscalls only | time inside syscalls, without the programs a CPI starts |
| time by syscall | the same, per syscall |
| cost per compute unit, per syscall and per program run | a least squares fit. Its inputs are correlated, so read differences between modes and not single numbers |
| by program | instruction time grouped by a transaction's first program |
| by SBPF version of the executing program | instruction time in v0, v1, v2 and v3 programs, per mode |
| by what the transaction's programs call | parrot jit against legacy jit, for transactions grouped by the calls parrot had to resolve at run time |

### 4.5 What is timed

Two clocks in `solana-sbpf` (feature `execution-timing`) split the time inside a top-level
program run into two parts.

- **Instruction time** is the time inside `EbpfVm::execute_program`, for the top-level program
  and for every program it calls into, minus the syscalls made from it.
- **Syscall time** is the time inside syscalls, minus the program runs a CPI starts. For a CPI
  it is everything around the callee's instructions, which is translating accounts,
  serializing the callee's input, creating its VM and copying the results back.

Neither includes what happens before and after a top-level run, which is account loading,
serialization, VM creation and compilation.

Timing a syscall costs two clock reads. The bench measures that cost at startup and takes it
out of every figure.

### 4.6 Checking the replay against mainnet

Two more commands help when the replay and mainnet disagree.

```sh
target/release/seashell-replay fidelity $DATA/blocks $DATA/accounts.bin
target/release/seashell-replay diagnose $DATA/blocks $DATA/accounts.bin
```

`fidelity` compares every mode with mainnet, with the distribution of compute unit differences
and the program each mismatching transaction first differs in.

`diagnose` checks the lamports and token amounts every transaction starts from against what
mainnet recorded. That separates a transaction that was given wrong state from one that
executes differently on the right state.

On the 100 slots after slot 470154816 the replay matched mainnet exactly on 38,556 of 38,628
transactions.

## 5. What the replay does not model

- The SlotHashes sysvar stays as the snapshot had it. Advancing it needs bank hashes, which
  `getBlock` does not return. Programs that read it can diverge.
- Checks the runtime makes on a whole transaction after it ran, such as rent state.
- Fees paid out to block leaders at the end of a block.
- A program deployed during the replay is not callable afterwards.
- Signatures, the transaction's blockhash and address lookup tables are not verified. Lookup
  table addresses are taken from the block's metadata.

## 6. Known limits of the measurement

- The harness rebuilds the syscall registry and copies the program cache for every
  transaction, so each transaction starts with cold caches.
- One core, pinned but not isolated from the scheduler.
- No hardware counters are used.
- The sbpf fork carries experiment code that is off by default. That is the call dispatch
  cache and counters (`src/codegen/call_dispatch.rs`), the step profile
  (`src/step_profile.rs`) and the interpreter step layouts selected with
  `PARROT_STEP_LAYOUT`. [PARROT_DIAGNOSTICS.md](PARROT_DIAGNOSTICS.md) says what each showed.
  The call dispatch counters always count. The cache is off unless the
  `call-dispatch-experiment` subcommand turns it on.
