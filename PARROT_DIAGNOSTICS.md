# Two parrot regressions on mainnet programs

Measured by replaying 100 mainnet slots (470154817 to 470154916, 38,628 transactions, 539
programs) under the legacy engines and under parrot. [PARROT.md](PARROT.md) explains the setup.

Both regressions below are real and both show up on programs that run on mainnet today. They
have different causes and hit different programs.

| | JIT regression | Interpreter regression |
|---|---|---|
| Engine | parrot jit against legacy jit | parrot interp against legacy interp |
| Cause | calls to a program's own functions are resolved at run time | interpreter steps collide in the L1 instruction cache |
| Which programs | SBPF v0, v1 and v2 programs that make function calls | programs whose hot code uses many register combinations of the same kind of instruction, in any SBPF version |
| What decides it | how the program was built | what the program's code looks like |
| Size on these 100 slots | parrot jit is 0.88x of legacy jit on instruction time, 0.92x with syscalls | 0.79x on the 16 worst programs. 1.02x over all programs, because the others gain |
| Suggested remediation | resolve near calls at compile time for pre-v3 programs. Not tried yet | a step layout that mixes the registers into the cache set. Tried, 1.02x becomes 1.11x |

A program can have one, both or neither. Falcon-512 verification is built as v3 and is mostly
register arithmetic. It has no JIT regression (parrot jit is 1.14x of legacy jit) and a large
interpreter regression (0.79x). A typical v0 DeFi program has the JIT regression and is about
level under the interpreter.

## 1. JIT: near calls in pre-v3 programs

### What happens

- A program calls its own functions with `call`. The compiler emits these as relative calls.
- For v0, v1 and v2 programs the sbpf loader rewrites every one of them into `call <hash>` and
  records the hash in a table (`elf.rs`, the relative call fixup). After loading, a call to
  the program's own function and a syscall look the same.
- The legacy JIT looks the hash up once, when it compiles, and emits a direct call.
- Parrot looks it up every time the instruction runs. It saves the BPF registers, calls into
  Rust (`dispatch_call`), searches the syscall table, then the program's function table,
  restores the registers and only then makes the call.
- In v3 the loader leaves near calls relative. Parrot handles them in generated code with no
  lookup.

One near call and return, in a tight loop.

| | legacy jit | parrot jit |
|---|---:|---:|
| v0 program | 1.2 ns | 7.5 ns |
| v3 program | 1.5 ns | 1.6 ns |

### Which mainnet programs

Any v0, v1 or v2 program that calls functions. That is most of mainnet. Of the 539 programs
active in these slots, 431 are v0, 3 are v1, 7 are v2 and 98 are v3. Half of the v0 programs
were last deployed within 40 days, so this is not a shrinking tail. In one pass they make
18.0 million near calls that parrot resolves at run time.

### Evidence

Instruction time by the version of the program that is executing.

| program version | legacy jit (ms) | parrot jit (ms) | parrot vs legacy | gap (ms) |
|---|---:|---:|---:|---:|
| v0 | 2200.8 | 2533.4 | 0.87x | +332.6 |
| v1 | 15.5 | 19.0 | 0.82x | +3.4 |
| v2 | 18.4 | 20.8 | 0.88x | +2.4 |
| v3 | 251.5 | 261.9 | 0.96x | +10.4 |

The same time by what a transaction's programs do.

| transactions | txs | near calls | legacy jit (ms) | parrot jit (ms) | parrot vs legacy |
|---|---:|---:|---:|---:|---:|
| no calls and no syscalls | 7,545 | 0 | 9.7 | 10.3 | 0.94x |
| v3 programs only | 6,685 | 0 | 135.7 | 138.3 | 0.98x |
| pre-v3 programs that make near calls | 23,957 | 18,022,515 | 2331.4 | 2681.6 | 0.87x |

95% of the gap is inside v0 programs, and all of it is in transactions that make near calls.

### What is not settled

- Syscalls from pre-v3 programs take the same run-time path, so near calls and syscalls are
  not separated by these tables. Near calls are 83% of the calls on that path.
- The cost per call on mainnet works out to 18 to 23 ns. The tight loop shows 6 ns. The
  difference is unexplained.
- Answering the two lookups from a cache moves parrot jit from 0.88x to 0.90x, so the lookups
  are a small part of the cost. The rest of the path would have to be the remainder.

The direct test is to resolve near calls at compile time for pre-v3 programs in parrot and
measure again.

## 2. Interpreter: steps collide in the L1 instruction cache

### What happens

- Parrot's interpreter has a separate piece of code, a step, for every combination of opcode,
  destination register and source register. Each step has a 128-byte slot in an 8 MiB table,
  at `base + (op | dst << 8 | src << 12) * 128`.
- The L1 instruction cache on the test machine has 64 sets of 8 lines. It picks the set from
  address bits 6 to 11.
- With that layout those bits hold only the low five bits of the opcode. The registers never
  reach them.
- So every step for the same kind of instruction lands in the same set, whatever registers it
  uses. All 64-bit register-to-register arithmetic falls into just two sets.
- A set holds 8 lines. When a loop uses more than 8 steps from one set, they evict each other
  on every pass, and each instruction waits for its step to come back from L2.

The legacy interpreter has one handler per opcode and treats registers as data, so its code is
a few KiB and fits. The JITs lay code out in program order, which spreads it over all sets.

One opcode executed with N different register pairs, time per instruction under parrot interp.

| N | 4 | 8 | 12 | 16 | 64 |
|---|---:|---:|---:|---:|---:|
| ns | 0.41 | 0.37 | 1.98 | 2.81 | 3.93 |

The step is at 8, the number of lines in a set. The legacy interpreter stays near 1.6 ns
throughout.

### Which mainnet programs

Programs whose hot code needs more steps per cache set than the set holds. The SBPF version
does not matter. To measure it, the replay counts what each program executes and computes its
**overflow**, the share of its executed instructions whose step is not among the 8 most
executed steps of its set.

| overflow | programs | legacy interp (ms) | parrot interp (ms) | parrot vs legacy |
|---|---:|---:|---:|---:|
| under 5% | 23 | 10.5 | 8.5 | 1.24x |
| 5% to 15% | 39 | 107.6 | 80.0 | 1.35x |
| 15% to 30% | 188 | 1567.6 | 1475.8 | 1.06x |
| 30% to 50% | 261 | 3324.2 | 3330.9 | 1.00x |
| 50% and over | 16 | 87.7 | 111.0 | 0.79x |
| all | 527 | 5097.6 | 5006.3 | 1.02x |

Parrot's interpreter is faster where the steps fit, level where about a third to a half of the
instructions overflow, and slower beyond that. Most mainnet instruction time sits in the level
group, which is why the total comes out at 1.02x.

Programs where it is clearly slower.

| program | version | overflow | legacy interp (ms) | parrot interp (ms) | parrot vs legacy |
|---|---|---:|---:|---:|---:|
| `HVi6VyyLvTtFTA8f8atavxVjUKi8WjmnydfKgoZKzt7H` | v2 | 52% | 23.9 | 36.1 | 0.66x |
| `DRVSpZ2YUYYKgZP8XtLhAGtT1zYSCKzeHfb4DgRnrgqD` | v1 | 52% | 34.2 | 42.2 | 0.81x |
| `MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms` | v3 | 43% | 9.9 | 11.6 | 0.85x |
| `L2TExMFKdjpN9kozasaurPirfHy9P8sbXoAN1qA3S95` | v0 | 31% | 21.8 | 25.1 | 0.87x |

And one where it is clearly faster, for contrast.

| program | version | overflow | legacy interp (ms) | parrot interp (ms) | parrot vs legacy |
|---|---|---:|---:|---:|---:|
| `BiSoNHVpsVZW2F7rx2eQ59yQwKxzU5NvBcmKshCSUypi` | v3 | 13% | 82.8 | 57.3 | 1.45x |

### Proof by changing the layout

If the cause is which steps share a cache set, then changing what picks the set has to move
the slowdown. The sbpf fork has a switch for that, `PARROT_STEP_LAYOUT`. It permutes the step
index and changes nothing else. The steps, the table size and the programs stay the same.

| layout | change to the dispatch code | what picks the cache set |
|---|---|---|
| `opcode-low` | none, the current behaviour | the opcode |
| `registers-low` | one instruction, swap the two bytes | the registers |
| `mixed` | one instruction, xor the registers into the opcode byte | both |

Two loops of N distinct instructions. The theory says swapping the bytes swaps which loop is
slow. Parrot interp, ns per instruction.

| loop body | N | `opcode-low` | `registers-low` | `mixed` |
|---|---:|---:|---:|---:|
| one opcode, N register pairs | 8 | 0.37 | 0.41 | 0.39 |
| one opcode, N register pairs | 12 | 1.98 | 0.40 | 0.38 |
| one opcode, N register pairs | 14 | 2.38 | 0.40 | 0.37 |
| N opcodes, one register pair | 8 | 0.37 | 0.43 | 0.40 |
| N opcodes, one register pair | 12 | 0.35 | 2.00 | 0.39 |
| N opcodes, one register pair | 14 | 0.35 | 2.37 | 0.38 |

The slowdown moves to the other loop at the same size, it still starts between 8 and 12, and
it is gone from both loops when neither can pile into one set. The legacy interpreter is 1.56
to 1.69 ns in every cell.

### Suggested remediation: the `mixed` layout

`mixed` adds one instruction to the interpreter's dispatch, `xor cl, ch`, and uses the same
permutation when the table is generated. Each layout is a permutation of the 16-bit values, so
the table keeps its size. sbpf's 152 execution tests, which compare the engines against each
other, pass under it.

Falcon-512 verification, p50 in microseconds. Only parrot's interpreter uses the step table.

| mode | `opcode-low` | `mixed` |
|---|---:|---:|
| legacy interp | 641 | 638 |
| legacy jit | 172 | 173 |
| parrot interp | 805 | 676 |
| parrot jit | 144 | 146 |

The 100 mainnet slots, parrot interp against legacy interp, by the overflow of the current
layout.

| overflow | programs | `opcode-low` | `mixed` |
|---|---:|---:|---:|
| under 5% | 23 | 1.24x | 1.30x |
| 5% to 15% | 39 | 1.35x | 1.45x |
| 15% to 30% | 188 | 1.06x | 1.14x |
| 30% to 50% | 261 | 1.00x | 1.09x |
| 50% and over | 16 | 0.79x | 0.84x |
| all | 527 | 1.02x | 1.11x |

Parrot's interpreter goes from 5,006 ms to 4,611 ms on this traffic while the legacy
interpreter stays where it was (5,098 and 5,115 ms).

`mixed` is the first layout tried, not the best one found. It spreads the steps by opcode and
by registers, and nothing here says another permutation would not do better.

### What is not settled

- The test machine does not allow hardware counters, so no cache miss count backs the last
  link, that the shared address bits matter because they pick the L1 instruction cache set.
  That rests on the cache geometry and on the slowdown starting at the number of lines in a
  set.
- `mixed` does not remove the regression everywhere. The worst group is still at 0.84x and
  falcon at 0.94x. A program with a thousand hot steps cannot fit a 32 KiB cache under any
  layout.
- `HVi6VyyLvTtFTA8f8atavxVjUKi8WjmnydfKgoZKzt7H` barely moves, from 0.66x to 0.68x. Something
  else slows it down under parrot's interpreter.
- Overflow is a lower bound on what does not fit. It ignores the rest of the code that shares
  the cache.
- Pre-v3 programs also pay parrot's run-time call resolution under the interpreter, about 1 to
  2 ns more per near call than the legacy interpreter. That is mixed into the v0 rows.
- The harness starts every transaction with cold caches, which costs an 8 MiB table more than
  a small interpreter.

## Reproducing

```sh
# section 1, and the regular tables
RUST_LOG=error taskset -c 12 target/release/seashell-replay bench $DATA/blocks $DATA/accounts.bin 5

# section 2, in the current layout and in the suggested one
RUST_LOG=error taskset -c 12 target/release/seashell-replay interp-experiment $DATA/blocks $DATA/accounts.bin 5
PARROT_STEP_LAYOUT=mixed RUST_LOG=error taskset -c 12 target/release/seashell-replay interp-experiment $DATA/blocks $DATA/accounts.bin 5

# falcon in the suggested layout
PARROT_STEP_LAYOUT=mixed RUST_LOG=error taskset -c 12 target/release/deps/falcon_execution_modes-<hash> \
    --ignored --nocapture --test-threads 1 falcon_execution_modes
```

`PARROT_STEP_LAYOUT` is read once per process, when the interpreter is generated.

The loop tables come from `tests/parrot_probes.rs` in the sbpf fork
(github.com/soundsonacid/sbpf, branch `frank/parrot`).

```sh
cargo test --release --test parrot_probes --no-run
taskset -c 12 target/release/deps/parrot_probes-<hash> --ignored --nocapture internal_call_cost
taskset -c 12 target/release/deps/parrot_probes-<hash> --ignored --nocapture same_opcode_sweep
PARROT_STEP_LAYOUT=registers-low taskset -c 12 target/release/deps/parrot_probes-<hash> \
    --ignored --nocapture layout_predictions
```
