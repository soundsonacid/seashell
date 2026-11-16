thank you to @ptaffet-jump for the initial syscall structure, @cavemanloverboy for helping with the design & discussion post

# infinite arithmetic

math today is expensive. we do not yet have native support for 128-bit math, and we have no support for any sort of data-level parallelism. the incredible advancements made by hardware designers, available on modern validator hardware, still elude the solana program developer. this forces many protocols to decide between efficiency & sophistication, stifling innovation from happening on chain.

despite the great work done by blueshift on [simd-0645](https://github.com/solana-foundation/solana-improvement-documents/pull/645) and `sol_multi3`, high-performance 128-bit multiplication is but a small subset of the operations we must improve to bring increasingly sophisticated protocols to solana. we must empower developers to write complex mathematical operations on chain with unprecedented efficiency by unlocking SIMD at a vm level, enabling a wholly new set of products to be built on solana.

## sol_math

we propose introducing `sol_math`, a generalizable math syscall.

```c
/* sol_math: evaluates a sequence of up to 10 arithmetic / logical
   operations over up to 3 scalar or vector inputs.

   op_sequence - up to 10 6-bit opcodes in reverse polish notation,
                 plus 4 flag bits in the low nibble
   cnt         - number of lanes / length of the input vectors. ignored for
                 scalar inputs
   inputs      - up to 3 input scalars or buffers. width determined by
                 the op_sequence integer width flag
   output      - scalar or vector output buffer. may alias one of the
                 input buffers. width determined by the op_sequence
                 integer width flag */
void
sol_math( ulong  op_sequence,
          ulong  cnt,
          void * inputs[3],
          void * output );
```

### metering

the cost of a call is statically determined before any work is done. it depends only on the op sequence and `cnt`, so the [proof-of-concept implementation](https://github.com/soundsonacid/agave/tree/sol-math) calculates & consumes the cost immediately after decoding the op sequence. this prevents the validator from wasting immense amounts of time working through trillion-element slices. a `cnt` larger than the buffers actually passed also fails before any arithmetic.

the charge itself is a fixed base cost per call plus a cost per 8-lane chunk, proportional to the number of vector instructions the op sequence executes. further work on the cost model could expand to pricing avx instructions more in accordance with their cost. the current cost model charges each instruction 4 cu.

further detail regarding the proposed design is available in the [benchmark repo](https://github.com/soundsonacid/seashell/tree/sol-math/programs/sol-math).

## initial results

the results from this design are quite promising. at 4096 lanes per call, across the set of proposed operations we see a wallclock speedup of 11.7x-47.6x for 64-bit integers and 10.3x-45.1x for 128-bit integers. the proposed cost model results in a 6.1x-20.0x reduction in cu consumption for 64-bit operations and an 11.1x-60.5x reduction for 128-bit operations. for several operations, cu-per-ns performance of `sol_math` exceeds that of the sbpf jit, indicating that even further cu reductions are justifiable. all benchmarks were run on an epyc 9275f.

### additional operations

enabling new operations is trivially simple. as opposed to introducing a new jit intrinsic or syscall to support a desired operation, the framework proposed here only requires an additional opcode to be accepted by the syscall's parser, a price, and an execution path added to the interpreter dispatch.

outside of the operations laid out in the poc, there are several other operations which would likely be valuable.

- full-width multiply (`vpmullq` / avx-512-dq)
- rounding-up muldiv (amms round against the user)
- checked / saturating addition / subtraction / multiply
- unsigned `MIN` / `MAX`
- integer sqrt
- bit counts (`vplzcntq` / avx-512-cd, `vpopcntq` / avx-512-vpopcntdq)
