use solana_pubkey::{pubkey, Pubkey};

use crate::symbolicate::Symbolicator;
use crate::Seashell;

pub const TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const TOKEN_2022_PROGRAM_ID: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

pub fn load(seashell: &mut Seashell) {
    for (program_id, name, so, debug) in [
        (
            TOKEN_PROGRAM_ID,
            "p_token",
            include_bytes!("elfs/ptoken.so").as_slice(),
            include_bytes!("elfs/ptoken.debug").as_slice(),
        ),
        (
            ASSOCIATED_TOKEN_PROGRAM_ID,
            "associated_token",
            include_bytes!("elfs/associated_token.so").as_slice(),
            include_bytes!("elfs/associated_token.debug").as_slice(),
        ),
        (
            TOKEN_2022_PROGRAM_ID,
            "token22",
            include_bytes!("elfs/token22.so").as_slice(),
            include_bytes!("elfs/token22.debug").as_slice(),
        ),
    ] {
        seashell.load_program_from_bytes(program_id, so);
        let mut symbolicator = Symbolicator::new();
        match symbolicator.load_elf_symbols(debug) {
            Ok(n) => log::debug!("symbolicator: loaded {n} function symbols for '{name}'"),
            Err(e) => log::debug!("symbolicator: no symbols for embedded '{name}': {e}"),
        }
        seashell.symbolicators.insert(program_id, symbolicator);
        seashell.program_names.insert(program_id, name.to_string());
    }
}
