//! Reads accounts out of Solana snapshot archives (`.tar.zst`) with Firedancer's streaming
//! snapshot parser, `fd_ssparse`.
//!
//! The archive is decompressed and parsed as one stream and nothing is written to disk. Only the
//! accounts asked for are kept, so memory use is the size of those accounts.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub type Address = [u8; 32];

/// One account as stored in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotAccount {
    /// The slot the account was last written in.
    pub slot: u64,
    pub lamports: u64,
    pub owner: Address,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScanProgress {
    /// Bytes of the decompressed archive parsed so far.
    pub uncompressed_bytes: u64,
    /// Account entries parsed so far. An account written in several slots has several entries.
    pub accounts_seen: u64,
    /// Entries that were among the wanted addresses.
    pub accounts_matched: u64,
}

/// Progress is reported every this many decompressed bytes.
const PROGRESS_INTERVAL_BYTES: u64 = 8 << 30;
const READ_BUFFER_BYTES: usize = 8 << 20;

/// Reads `archive` front to back and puts every account whose address is in `wanted` into
/// `found`.
///
/// An account that is already in `found` is replaced only by an entry from the same or a later
/// slot. So, to get the state at an incremental snapshot, call this for the full snapshot first
/// and then for the incremental one with the same `found`.
///
/// An account that was deleted shows up with zero lamports, if it shows up at all.
pub fn extract_accounts(
    archive: &Path,
    wanted: &HashSet<Address>,
    found: &mut HashMap<Address, SnapshotAccount>,
    mut on_progress: impl FnMut(&ScanProgress),
) -> io::Result<ScanProgress> {
    let mut decoder = zstd::stream::read::Decoder::new(File::open(archive)?)?;
    let parser = Parser::new();
    let mut buffer = vec![0u8; READ_BUFFER_BYTES];
    let mut progress = ScanProgress::default();
    let mut next_progress_report = PROGRESS_INTERVAL_BYTES;
    // The wanted account whose data is still arriving.
    let mut partial: Option<(Address, SnapshotAccount, usize)> = None;

    loop {
        let filled = decoder.read(&mut buffer)?;
        if filled == 0 {
            return Err(invalid(archive, "the archive ended before the end of the snapshot"));
        }
        let mut offset = 0;
        let mut stalled_calls = 0;
        while offset < filled {
            let mut event = Event::default();
            // SAFETY: the pointer and length describe the unparsed rest of `buffer`, and the
            // parser was created by `seashell_ssparse_new`.
            let kind = unsafe {
                seashell_ssparse_advance(
                    parser.0,
                    buffer.as_ptr().add(offset),
                    (filled - offset) as u64,
                    &mut event,
                )
            };
            let consumed = event.bytes_consumed as usize;
            offset += consumed;
            progress.uncompressed_bytes += consumed as u64;

            // The parser changes state without consuming input now and then, but not forever.
            stalled_calls = if consumed == 0 { stalled_calls + 1 } else { 0 };
            if stalled_calls > 64 {
                return Err(invalid(archive, "the snapshot parser stopped making progress"));
            }

            match kind {
                ADVANCE_ERROR => return Err(invalid(archive, "malformed snapshot")),
                ADVANCE_DONE => return Ok(progress),
                ADVANCE_ACCOUNT_HEADER => {
                    progress.accounts_seen += 1;
                    // SAFETY: on an account header the parser hands out pointers to 32 bytes
                    // each that stay valid until the next call.
                    let (address, owner) = unsafe {
                        (*(event.pubkey as *const Address), *(event.owner as *const Address))
                    };
                    if !wanted.contains(&address) {
                        continue;
                    }
                    progress.accounts_matched += 1;
                    let data_len = event.data_len as usize;
                    let account = SnapshotAccount {
                        slot: event.slot,
                        lamports: event.lamports,
                        owner,
                        executable: event.executable != 0,
                        rent_epoch: event.rent_epoch,
                        data: Vec::with_capacity(data_len),
                    };
                    if data_len == 0 {
                        keep_newest(found, address, account);
                    } else {
                        partial = Some((address, account, data_len));
                    }
                }
                ADVANCE_ACCOUNT_DATA => {
                    let Some((_, account, data_len)) = partial.as_mut() else {
                        continue;
                    };
                    // SAFETY: on account data the parser points into the input it was given.
                    let piece =
                        unsafe { std::slice::from_raw_parts(event.data, event.data_sz as usize) };
                    account.data.extend_from_slice(piece);
                    if account.data.len() == *data_len {
                        let (address, account, _) = partial.take().unwrap();
                        keep_newest(found, address, account);
                    }
                }
                _ => {}
            }
        }
        if progress.uncompressed_bytes >= next_progress_report {
            next_progress_report += PROGRESS_INTERVAL_BYTES;
            on_progress(&progress);
        }
    }
}

fn keep_newest(
    found: &mut HashMap<Address, SnapshotAccount>,
    address: Address,
    account: SnapshotAccount,
) {
    match found.get(&address) {
        Some(existing) if existing.slot > account.slot => {}
        _ => {
            found.insert(address, account);
        }
    }
}

fn invalid(archive: &Path, what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{}: {what}", archive.display()))
}

// FD_SSPARSE_ADVANCE_* in firedancer/src/discof/restore/utils/fd_ssparse.h
const ADVANCE_ERROR: i32 = -1;
const ADVANCE_ACCOUNT_HEADER: i32 = 4;
const ADVANCE_ACCOUNT_DATA: i32 = 5;
const ADVANCE_DONE: i32 = 8;

/// `struct seashell_ssparse_event` in c/seashell_ssparse.c.
#[repr(C)]
struct Event {
    bytes_consumed: u64,
    slot: u64,
    data_len: u64,
    lamports: u64,
    rent_epoch: u64,
    pubkey: *const u8,
    owner: *const u8,
    executable: i32,
    data: *const u8,
    data_sz: u64,
}

impl Default for Event {
    fn default() -> Self {
        Event {
            bytes_consumed: 0,
            slot: 0,
            data_len: 0,
            lamports: 0,
            rent_epoch: 0,
            pubkey: std::ptr::null(),
            owner: std::ptr::null(),
            executable: 0,
            data: std::ptr::null(),
            data_sz: 0,
        }
    }
}

/// An `fd_ssparse_t`.
#[repr(C)]
struct FdSsparse {
    _opaque: [u8; 0],
}

struct Parser(*mut FdSsparse);

impl Parser {
    fn new() -> Self {
        // SAFETY: no preconditions.
        let parser = unsafe { seashell_ssparse_new() };
        assert!(!parser.is_null(), "out of memory");
        Parser(parser)
    }
}

impl Drop for Parser {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `seashell_ssparse_new` and is freed once.
        unsafe { seashell_ssparse_delete(self.0) }
    }
}

extern "C" {
    fn seashell_ssparse_new() -> *mut FdSsparse;
    fn seashell_ssparse_delete(parser: *mut FdSsparse);
    fn seashell_ssparse_advance(
        parser: *mut FdSsparse,
        data: *const u8,
        data_sz: u64,
        event: *mut Event,
    ) -> i32;
}
