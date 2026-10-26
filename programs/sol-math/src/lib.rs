//! Exercises the `sol_math` syscall and, for comparison, the same arithmetic written as
//! ordinary BPF loops.
//!
//! Instruction data (little-endian): byte 0 = pattern, bytes 8..16 = op word,
//! bytes 16..24 = lane count. The account holds four equal regions `a | b | c | out`, each
//! `lane_count * lane_bytes` long, with `lane_bytes` = 8 or 16 from bit 0 of the op word.
//! Optional bytes 24..32 = repeat count (default 1): the pattern runs that many times in one
//! instruction, so a benchmark can fit time against repeats and drop the VM setup.

pub const PATTERN_NOOP: u8 = 0;
/// `sol_math(op_word, lane_count, [a, b, c], out)`.
pub const PATTERN_SOL_MATH: u8 = 1;
/// BPF loop: `out[i] = ((a[i] * b[i]) as u128 >> c[0]) as u64`.
pub const PATTERN_BPF_MUL128_SHR: u8 = 2;
/// BPF loop: `out[i] = a[i] + (((b[i] - a[i]) as u128 * c[i] as u128) >> 52) as u64`.
pub const PATTERN_BPF_LERP52: u8 = 3;
/// BPF loop: `out[i] = a[i] + b[i]`, the cheapest loop there is.
pub const PATTERN_BPF_ADD: u8 = 4;
/// BPF loop: `out[i] = ((a[i] as u128 * b[i] as u128) / c[0] as u128) as u64`.
pub const PATTERN_BPF_MUL_DIV: u8 = 5;
/// The u128 forms of the four loops above, on 16-byte lanes (the op word must set bit 0 so the
/// regions are sized for them). Products are the full 256 bits, truncated to 128 at the end.
/// BPF loop: `out[i] = a[i] + b[i]`.
pub const PATTERN_BPF_ADD_U128: u8 = 6;
/// BPF loop: `out[i] = (a[i] * b[i]) >> c[0]`.
pub const PATTERN_BPF_MUL128_SHR_U128: u8 = 7;
/// BPF loop: `out[i] = (a[i] * b[i]) / c[0]`; a divisor that fits in u64 takes four u128-by-u64
/// divides, any other the 256-step shift-subtract the host scalar path uses.
pub const PATTERN_BPF_MUL_DIV_U128: u8 = 8;
/// BPF loop: `out[i] = a[i] + (((b[i] - a[i]) & (2^52 - 1)) * (c[i] & (2^52 - 1)) >> 52)`, what
/// `[A, B, A, SUB, C, MUL_HI52, ADD]` computes on u128 lanes.
pub const PATTERN_BPF_LERP52_U128: u8 = 9;

pub const OP_WORD_OFFSET: usize = 8;
pub const LANE_COUNT_OFFSET: usize = 16;
pub const INSTRUCTION_LEN: usize = 24;
pub const REPEATS_OFFSET: usize = 24;

#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use super::*;
    use pinocchio::account_info::AccountInfo;
    use pinocchio::entrypoint;
    use pinocchio::program_error::ProgramError;
    use pinocchio::pubkey::Pubkey;
    use pinocchio::ProgramResult;
    use solana_define_syscall::define_syscall;

    define_syscall!(fn sol_math(op_word: u64, lane_count: u64, inputs: *const *const u8, output: *mut u8) -> u64);

    entrypoint!(process_instruction);

    pub fn process_instruction(
        _program_id: &Pubkey,
        accounts: &[AccountInfo],
        instruction_data: &[u8],
    ) -> ProgramResult {
        let [buffers] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };
        if instruction_data.len() < INSTRUCTION_LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        let pattern = instruction_data[0];
        let op_word = u64::from_le_bytes(instruction_data[OP_WORD_OFFSET..OP_WORD_OFFSET + 8].try_into().unwrap());
        let lane_count = u64::from_le_bytes(instruction_data[LANE_COUNT_OFFSET..LANE_COUNT_OFFSET + 8].try_into().unwrap());
        let repeats = match instruction_data.get(REPEATS_OFFSET..REPEATS_OFFSET + 8) {
            Some(bytes) => u64::from_le_bytes(bytes.try_into().unwrap()),
            None => 1,
        };
        let lane_bytes = if op_word & 1 != 0 { 16 } else { 8 };
        let region_bytes = lane_count as usize * lane_bytes;

        let mut data = buffers.try_borrow_mut_data()?;
        if data.len() < 4 * region_bytes {
            return Err(ProgramError::AccountDataTooSmall);
        }
        let (a, rest) = data.split_at_mut(region_bytes);
        let (b, rest) = rest.split_at_mut(region_bytes);
        let (c, rest) = rest.split_at_mut(region_bytes);
        let out = &mut rest[..region_bytes];

        for _ in 0..repeats {
            run_pattern(pattern, op_word, lane_count, a, b, c, out)?;
        }
        Ok(())
    }

    #[inline(always)]
    fn run_pattern(
        pattern: u8,
        op_word: u64,
        lane_count: u64,
        a: &[u8],
        b: &[u8],
        c: &[u8],
        out: &mut [u8],
    ) -> ProgramResult {
        match pattern {
            PATTERN_NOOP => {}
            PATTERN_SOL_MATH => {
                let inputs: [*const u8; 3] = [a.as_ptr(), b.as_ptr(), c.as_ptr()];
                let status = unsafe { sol_math(op_word, lane_count, inputs.as_ptr(), out.as_mut_ptr()) };
                if status != 0 {
                    return Err(ProgramError::Custom(status as u32));
                }
            }
            PATTERN_BPF_MUL128_SHR => {
                let shift = read_u64(c, 0) as u32;
                for lane in 0..lane_count as usize {
                    let product = read_u64(a, lane) as u128 * read_u64(b, lane) as u128;
                    let shifted = if shift >= 128 { 0 } else { (product >> shift) as u64 };
                    write_u64(out, lane, shifted);
                }
            }
            PATTERN_BPF_LERP52 => {
                for lane in 0..lane_count as usize {
                    let (low, high, fraction) = (read_u64(a, lane), read_u64(b, lane), read_u64(c, lane));
                    let delta = high.wrapping_sub(low) as u128 * fraction as u128;
                    write_u64(out, lane, low.wrapping_add((delta >> 52) as u64));
                }
            }
            PATTERN_BPF_ADD => {
                for lane in 0..lane_count as usize {
                    write_u64(out, lane, read_u64(a, lane).wrapping_add(read_u64(b, lane)));
                }
            }
            PATTERN_BPF_MUL_DIV => {
                let divisor = read_u64(c, 0) as u128;
                if divisor == 0 {
                    return Err(ProgramError::InvalidArgument);
                }
                for lane in 0..lane_count as usize {
                    let quotient = read_u64(a, lane) as u128 * read_u64(b, lane) as u128 / divisor;
                    write_u64(out, lane, quotient as u64);
                }
            }
            PATTERN_BPF_ADD_U128 => {
                for lane in 0..lane_count as usize {
                    write_u128(out, lane, read_u128(a, lane).wrapping_add(read_u128(b, lane)));
                }
            }
            PATTERN_BPF_MUL128_SHR_U128 => {
                let shift = read_u128(c, 0);
                for lane in 0..lane_count as usize {
                    let product = mul_wide(read_u128(a, lane), read_u128(b, lane));
                    write_u128(out, lane, shift_right_wide(product, shift));
                }
            }
            PATTERN_BPF_MUL_DIV_U128 => {
                let divisor = read_u128(c, 0);
                if divisor == 0 {
                    return Err(ProgramError::InvalidArgument);
                }
                for lane in 0..lane_count as usize {
                    let product = mul_wide(read_u128(a, lane), read_u128(b, lane));
                    let quotient = if divisor >> 64 == 0 {
                        divide_wide_by_u64(product, divisor as u64)
                    } else {
                        divide_wide(product, divisor)
                    };
                    write_u128(out, lane, quotient);
                }
            }
            PATTERN_BPF_LERP52_U128 => {
                const MASK_52: u128 = (1 << 52) - 1;
                for lane in 0..lane_count as usize {
                    let (low, high, fraction) = (read_u128(a, lane), read_u128(b, lane), read_u128(c, lane));
                    let delta = (high.wrapping_sub(low) & MASK_52) * (fraction & MASK_52);
                    write_u128(out, lane, low.wrapping_add(delta >> 52));
                }
            }
            _ => return Err(ProgramError::InvalidInstructionData),
        }
        Ok(())
    }

    /// Full 256-bit product as four u64 limbs, least significant first.
    fn mul_wide(x: u128, y: u128) -> [u64; 4] {
        let (x0, x1, y0, y1) = (x as u64 as u128, x >> 64, y as u64 as u128, y >> 64);
        let (p00, p01, p10, p11) = (x0 * y0, x0 * y1, x1 * y0, x1 * y1);
        let middle = (p00 >> 64) + (p01 as u64 as u128) + (p10 as u64 as u128);
        let high = (p01 >> 64) + (p10 >> 64) + (p11 as u64 as u128) + (middle >> 64);
        [p00 as u64, middle as u64, high as u64, ((p11 >> 64) + (high >> 64)) as u64]
    }

    /// Bits `shift..shift + 128` of a 256-bit value; 0 once the shift reaches 256.
    fn shift_right_wide(limbs: [u64; 4], shift: u128) -> u128 {
        if shift >= 256 {
            return 0;
        }
        let (word, bit) = ((shift / 64) as usize, (shift % 64) as u32);
        let limb = |index: usize| limbs.get(index).copied().unwrap_or(0);
        let funnel = |low: u64, high: u64| if bit == 0 { low } else { (low >> bit) | (high << (64 - bit)) };
        let low = funnel(limb(word), limb(word + 1));
        let high = funnel(limb(word + 1), limb(word + 2));
        (high as u128) << 64 | low as u128
    }

    /// Low 128 bits of a 256-bit value divided by a u64, one u128-by-u64 divide per limb.
    fn divide_wide_by_u64(limbs: [u64; 4], divisor: u64) -> u128 {
        let mut remainder = 0u64;
        let mut quotient = [0u64; 4];
        for index in (0..4).rev() {
            let current = (remainder as u128) << 64 | limbs[index] as u128;
            quotient[index] = (current / divisor as u128) as u64;
            remainder = (current % divisor as u128) as u64;
        }
        (quotient[1] as u128) << 64 | quotient[0] as u128
    }

    /// Low 128 bits of a 256-bit value divided by a u128, by shift-subtract long division.
    fn divide_wide(limbs: [u64; 4], divisor: u128) -> u128 {
        let (high, low) = ((limbs[3] as u128) << 64 | limbs[2] as u128, (limbs[1] as u128) << 64 | limbs[0] as u128);
        let mut remainder = 0u128;
        let mut quotient = 0u128;
        for bit in (0..256u32).rev() {
            let next_bit = if bit >= 128 { (high >> (bit - 128)) & 1 } else { (low >> bit) & 1 };
            let overflowed = remainder >> 127 != 0;
            remainder = (remainder << 1) | next_bit;
            if overflowed || remainder >= divisor {
                remainder = remainder.wrapping_sub(divisor);
                quotient = (quotient << 1) | 1;
            } else {
                quotient <<= 1;
            }
        }
        quotient
    }

    fn read_u128(region: &[u8], lane: usize) -> u128 {
        u128::from_le_bytes(region[lane * 16..lane * 16 + 16].try_into().unwrap())
    }

    fn write_u128(region: &mut [u8], lane: usize, value: u128) {
        region[lane * 16..lane * 16 + 16].copy_from_slice(&value.to_le_bytes());
    }

    fn read_u64(region: &[u8], lane: usize) -> u64 {
        u64::from_le_bytes(region[lane * 8..lane * 8 + 8].try_into().unwrap())
    }

    fn write_u64(region: &mut [u8], lane: usize, value: u64) {
        region[lane * 8..lane * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }
}
