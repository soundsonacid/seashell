# sol-math

this is poc implementation of a generalizable math intrinsic / syscall.

thank u to phillip from fd for the syscall structure! :D

## run demo

```bash
cd seashell
./scripts/run-math-demo
```

## agave fork syscall implementation

the syscall implementation is available [here](https://github.com/soundsonacid/agave/commit/83b24ea5bcd4dd31a64af619f4d5e26ccf35af2b)

## syscall signature
```c
sol_math( ulong op_sequence, ulong cnt, void * inputs[3], void * output )
```

## formal defns
1. `op_sequence` - a 64-bit integer containing up to 10 6-bit operations, encoded with polish notation.
    the remaining four (low nibble) bits are used for various flags:
    bit 0 - integer width. 0 = `u64`, 1 = `u128`. input / output buffers are of this type & intermediate math is also done through this type. there is no mixing of integer widths.
    bit 1 - scalar vs vector flag for `inputs[0]`
    bit 2 - scalar vs vector flag for `inputs[1]`
    bit 3 - scalar vs vector flag for `inputs[2]`
2. `cnt` - lane width / length of input vectors. does not apply to scalar inputs
3. `inputs` - up to 3 input scalars or buffers. width determined by `op_sequence` flag
4. `output` - vector or scalar output buffer. may be the same as one of the input buffers. width determined by `op_sequence` flag

## basic behavior:

```
for op, i in op_sequence.enumerate():
    output[i] = do_op(inputs[0][i], inputs[1][i], inputs[2][i])
```

## enabled operations & encoding

the operations in group `11` accept 3 arguments.

```
      dec  bin      class    stack
END     0  00 0000  push     terminates the sequence
A       1  00 0001  push +1  inputs[0] lane (or broadcast)
B       2  00 0010  push +1  inputs[1]
C       3  00 0011  push +1  inputs[2]
LIT     4  00 0100  push +1  next token is the value, 0..63
DUP     5  00 0101  push +1  copy of top

NOT    16  01 0000  unary 0  !x
NEG    17  01 0001  unary 0  -x (wrapping)

ADD    32  10 0000  binary -1  x + y (wrapping)
SUB    33  10 0001             x - y (wrapping)
AND    34  10 0010             x & y
OR     35  10 0011             x | y
XOR    36  10 0100             x ^ y
MIN    37  10 0101             signed min
MAX    38  10 0110             signed max
LSH    39  10 0111             x << y          (y >= width -> 0)
LRSH   40  10 1000             x >> y logical  (y >= width -> 0)
ARSH   41  10 1001             x >> y arithmetic (y >= width -> sign fill)
DIV    42  10 1010             x / y unsigned  (y == 0 -> syscall error)
MOD    43  10 1011             x % y unsigned  (y == 0 -> syscall error)
MUL_LO52 44 10 1100            low 52 bits of (x & 2^52-1) * (y & 2^52-1)
MUL_HI52 45 10 1101            bits 52..103 of the same product

MUL128_DIV 48 11 0000  ternary -2  (x * y) / z, double-width product, truncated to width (z == 0 -> error)
MUL128_SHR 49 11 0001             (x * y) >> z, double-width product, truncated to width (z >= 2*width -> 0)
SELECT     50 11 0010             x != 0 ? y : z
```

## execution

this implementation does not actually lower to hardware operations directly. the syscall is an interpreter for the op sequence & is run on the host machine in native rust code.

if the `avx512f` and `avx512ifma` features are present AND the integer bit width is set to 0 (u64), `std::arch::x86_64` intrinsics will be used when applicable. a single intrinsic is used for all operations except the following operations:
- `MUL128_DIV`, `DIV`, `MOD` use none as there is not simd integer divison hardware support
- `A`, `B`, `C`, `DUP` use none
- `MUL128_SHR` uses a series of intrinsics
- `SELECT` uses two intrinsics

if the integer bit width is set to 1 (u128), or avx is not supported, standard rust arithmetic is used. it is (currently) assumed that the compiler will make reasonable choices for codegen.

note that there is concern around validators not having avx-512 support and increasing execution timing variability. the cost model also makes some assumptions around avx-512 support.

## cost model

```
FIXED_OP_COST = 4 // 3 loads + 1 store
MATH_BASE_COST + ceil(lane_count / 8) * MATH_CU_PER_THING * sum(things_done_per_op + FIXED_OP_COST)
```

`things_done_per_op` is the number of host instructions that the reference avx-512 impl executes to cdo the op for a single 8-lane chunk. this is kind of a guess for what is maybe reasonable because cus are made up.

exact things table is available [here](https://github.com/soundsonacid/agave/blob/83b24ea5bcd4dd31a64af619f4d5e26ccf35af2b/syscalls/src/math.rs#L148)

## demo results

```
test sol_math_matches_the_host_reference ...
lanes = 64, host path = avx512
| formula                  | sol_math | per lane |      bpf | per lane | bpf / sol_math |
|--------------------------|---------:|---------:|---------:|---------:|---------------:|
| a + b                    |      273 |     4.27 |     1159 |    18.11 |           4.2x |
| (a * b) >> 26            |     1041 |    16.27 |     5377 |    84.02 |           5.2x |
| a * b / 1e9              |      913 |    14.27 |    13503 |   210.98 |          14.8x |
| a + ((b - a) * f >> 52)  |      337 |     5.27 |     4601 |    71.89 |          13.7x |
| case             |       CU |   net CU | per lane |
|------------------|---------:|---------:|---------:|
| noop             |       61 |        0 |     0.00 |
| add              |      334 |      273 |     4.27 |
| sub              |      334 |      273 |     4.27 |
| min              |      334 |      273 |     4.27 |
| max              |      334 |      273 |     4.27 |
| lrsh_lit         |      366 |      305 |     4.77 |
| div              |      686 |      625 |     9.77 |
| mul_hi52         |      334 |      273 |     4.27 |
| mul128_shr       |     1102 |     1041 |    16.27 |
| mul128_div       |      974 |      913 |    14.27 |
| mul_div_1e9      |      974 |      913 |    14.27 |
| lerp52           |      398 |      337 |     5.27 |
| select_gt0       |      462 |      401 |     6.27 |
| u128 add         |      334 |      273 |     4.27 |
| u128 div         |      686 |      625 |     9.77 |
| u128 mul128_div  |      974 |      913 |    14.27 |
```