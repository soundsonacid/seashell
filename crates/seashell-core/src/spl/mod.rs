use solana_pubkey::{pubkey, Pubkey};

use crate::Seashell;

pub const TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const TOKEN_2022_PROGRAM_ID: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

pub fn load(seashell: &mut Seashell) {
    seashell.load_program_from_bytes(TOKEN_PROGRAM_ID, include_bytes!("elfs/tokenkeg.so"));
    seashell.load_program_from_bytes(
        ASSOCIATED_TOKEN_PROGRAM_ID,
        include_bytes!("elfs/associated_token.so"),
    );
    seashell.load_program_from_bytes(TOKEN_2022_PROGRAM_ID, include_bytes!("elfs/token22.so"));
}

pub fn load_p_token(seashell: &mut Seashell) {
    seashell.load_program_from_bytes(
        TOKEN_PROGRAM_ID,
        include_bytes!("elfs/ptoken.so"),
    );
}