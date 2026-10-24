//! Exercises the `sol_math` syscall and, for comparison, the same arithmetic written as
//! ordinary BPF loops.
//!
//! Instruction data (little-endian): byte 0 = pattern, bytes 8..16 = op word,
//! bytes 16..24 = lane count. The account holds four equal regions `a | b | c | out`, each
//! `lane_count * lane_bytes` long, with `lane_bytes` = 8 or 16 from bit 0 of the op word.

pub const PATTERN_NOOP: u8 = 0;
/// `sol_math(op_word, lane_count, [a, b, c], out)`.
pub const PATTERN_SOL_MATH: u8 = 1;
/// BPF loop: `out[i] = ((a[i] * b[i]) as u128 >> c[0]) as u64`.
pub const PATTERN_BPF_MUL128_SHR: u8 = 2;
/// BPF loop: `out[i] = a[i] + (((b[i] - a[i]) as u128 * c[i] as u128) >> 52) as u64`.
pub const PATTERN_BPF_LERP52: u8 = 3;

pub const OP_WORD_OFFSET: usize = 8;
pub const LANE_COUNT_OFFSET: usize = 16;
pub const INSTRUCTION_LEN: usize = 24;

#[cfg(feature = "bpf-entrypoint")]
mod entrypoint {
    use super::*;
    use pinocchio::account_info::AccountInfo;
    use pinocchio::entrypoint;
    use pinocchio::program_error::ProgramError;
    use pinocchio::pubkey::Pubkey;
    use pinocchio::ProgramResult;
    use solana_define_syscall::define_syscall;

    // sbpf v3 has no relocations: the syscall is a `syscall <murmur3(name)>` instruction,
    // which is what this macro emits (a const hash transmuted to a fn pointer).
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
            _ => return Err(ProgramError::InvalidInstructionData),
        }
        Ok(())
    }

    fn read_u64(region: &[u8], lane: usize) -> u64 {
        u64::from_le_bytes(region[lane * 8..lane * 8 + 8].try_into().unwrap())
    }

    fn write_u64(region: &mut [u8], lane: usize, value: u64) {
        region[lane * 8..lane * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }
}
