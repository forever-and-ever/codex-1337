//! Framed rollouts: one canonical `.jsonl.zst` file that a writer appends to without re-encoding.
//!
//! ```text
//! hint | frame trailer | frame trailer | ... | open frame
//! ```
//!
//! - The hint is a skippable frame at offset 0 naming the newest trailer. It is rewritten in place
//!   after each frame closes and is only a hint: readers verify the chain it names and walk
//!   forward from there over anything closed since.
//! - A frame is an ordinary zstd frame (window log 23, XXH64 content checksum, no content size).
//!   The writer flushes after every line, so block boundaries include every line boundary and
//!   every acknowledged line is decodable after a crash.
//! - A trailer is a skippable frame after each closed frame recording where the frame starts, the
//!   logical offset of its first byte, its logical length and the previous trailer.
//! - The open frame has no last block yet. The next writer seals it: it keeps the complete lines,
//!   copies the torn suffix to a durable `<file>.torn-<unix ms>` sidecar before truncating it, and
//!   ends the frame with an empty last block and the checksum. Sealing never destroys bytes.
//!
//! The whole file is a valid zstd stream. Every byte offset stored elsewhere (thread history,
//! fork prefixes) is logical: it addresses the decoded JSONL, as plain rollouts store directly.

mod quarantine;
mod reader;
mod survey;

pub(crate) use survey::read_first_frame;
mod writer;
mod xxh64;

use std::fs::File;
use std::io;
use std::time::Duration;

pub(crate) use reader::FramedReader;
pub(crate) use writer::FramedWriter;
pub(crate) use writer::reencode;

const HINT_MAGIC: u32 = 0x184D_2A5C;
const TRAILER_MAGIC: u32 = 0x184D_2A5D;
const FRAME_MAGIC: u32 = 0xFD2F_B528;
const FORMAT_TAG: [u8; 4] = *b"cxr1";
/// Magic and payload size, then the payload: format tag, reserved word, newest trailer offset.
const HINT_BODY_LEN: u32 = 16;
const HINT_LEN: usize = 8 + HINT_BODY_LEN as usize;
const HINT_TRAILER_FIELD: u64 = 16;
/// Magic and payload size, then the payload: frame start, logical start, logical length, previous
/// trailer offset.
const TRAILER_BODY_LEN: u32 = 32;
const TRAILER_LEN: usize = 8 + TRAILER_BODY_LEN as usize;
/// Frames close once they hold this many decoded bytes.
const FRAME_LOGICAL_BYTES: u64 = 16 << 20;
/// An open frame with no append for this long is closed, so a quiet rollout seals cheaply.
pub(crate) const IDLE_FRAME_CLOSE: Duration = Duration::from_mins(10);
const WINDOW_LOG: u32 = 23;
const COMPRESSION_LEVEL: i32 = 3;

/// Whether `file` begins with the framed layout's hint rather than a bare zstd frame.
pub(crate) fn is_framed(file: &File) -> io::Result<bool> {
    let mut head = [0; 12];
    match read_exact_at(file, &mut head, 0) {
        Ok(()) => Ok(u32_le(&head[..4]) == HINT_MAGIC && head[8..12] == FORMAT_TAG),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(err) => Err(err),
    }
}

fn u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap_or_default())
}

fn u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().unwrap_or_default())
}

/// An offset or length within a rollout as an in-memory index.
fn to_index(value: u64) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "rollout offset exceeds the address space",
        )
    })
}

fn zstd_error(code: usize) -> io::Error {
    io::Error::other(zstd::zstd_safe::get_error_name(code))
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buffer, offset)
}

#[cfg(unix)]
fn write_all_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buffer, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        match std::os::windows::fs::FileExt::seek_read(file, buffer, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            read => {
                buffer = &mut buffer[read..];
                offset += read as u64;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        let written = std::os::windows::fs::FileExt::seek_write(file, buffer, offset)?;
        buffer = &buffer[written..];
        offset += written as u64;
    }
    Ok(())
}

#[cfg(test)]
#[path = "framed_tests.rs"]
mod tests;
