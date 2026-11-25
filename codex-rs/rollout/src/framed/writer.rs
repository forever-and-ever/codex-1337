//! Appends lines to a framed rollout, closing frames by size or idleness and sealing on open.
//!
//! # Lock model
//!
//! A framed rollout's length changes only through its one open [`FramedWriter`], and a writer
//! exists only under its thread's cross-process writer lock:
//!
//! 1. The opener acquires the lock: a live or temporary `RolloutRecorder` holds its
//!    `WriterLockGuard` for the life of its writer task, `append_rollout_item_to_path` for the
//!    one append, and conversion and migration for the whole rewrite.
//! 2. [`FramedWriter::open`] seals whatever a previous writer left open, quarantining the cut
//!    bytes, and every append and frame close then extends the file at `end`.
//! 3. The writer closes its frame and is dropped before the lock is released. No writer releases
//!    the lock while it stays open, so no other writer can change the file under it.
//!
//! A length other than `end` before a write is therefore a broken lock discipline or a foreign
//! process. The writer refuses to write and logs at ERROR rather than re-sealing, which could cut
//! another writer's frame in flight.

use std::fs::File;
use std::io;
use std::io::BufRead;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use sha2::Digest;
use sha2::Sha256;

use zstd::zstd_safe::CCtx;
use zstd::zstd_safe::CParameter;
use zstd::zstd_safe::InBuffer;
use zstd::zstd_safe::OutBuffer;
use zstd::zstd_safe::zstd_sys::ZSTD_EndDirective;

use super::COMPRESSION_LEVEL;
use super::FRAME_LOGICAL_BYTES;
use super::HINT_LEN;
use super::HINT_TRAILER_FIELD;
use super::IDLE_FRAME_CLOSE;
use super::TRAILER_LEN;
use super::WINDOW_LOG;
use super::quarantine::quarantine;
use super::reader::FramedReader;
use super::survey::Remainder;
use super::survey::Span;
use super::survey::encode_hint;
use super::survey::encode_trailer;
use super::survey::survey;
use super::write_all_at;
use super::xxh64::xxh64;
use super::zstd_error;

/// An empty raw block with the last-block bit: what sealing appends to end an open frame.
const EMPTY_LAST_BLOCK: [u8; 3] = [1, 0, 0];

pub(crate) struct FramedWriter {
    path: PathBuf,
    file: File,
    encoder: CCtx<'static>,
    /// Where the next compressed byte goes.
    end: u64,
    logical_end: u64,
    last_trailer: u64,
    open: Option<OpenFrame>,
    frame_logical_bytes: u64,
    scratch: Vec<u8>,
}

#[derive(Clone, Copy)]
struct OpenFrame {
    start: u64,
    logical_start: u64,
    last_append: Instant,
}

impl FramedWriter {
    /// Takes `file`, open at `path` for reading and writing but not appending: an empty file
    /// receives the hint, and a framed one has its open frame sealed. Blocking; the caller holds
    /// the rollout's writer lock.
    pub(crate) fn open(path: &Path, file: File) -> io::Result<Self> {
        let mut encoder = CCtx::create();
        for parameter in [
            CParameter::CompressionLevel(COMPRESSION_LEVEL),
            CParameter::WindowLog(WINDOW_LOG),
            CParameter::ChecksumFlag(true),
        ] {
            encoder.set_parameter(parameter).map_err(zstd_error)?;
        }
        let mut writer = Self {
            path: path.to_path_buf(),
            file,
            encoder,
            end: HINT_LEN as u64,
            logical_end: 0,
            last_trailer: 0,
            open: None,
            frame_logical_bytes: FRAME_LOGICAL_BYTES,
            scratch: Vec::new(),
        };
        if writer.file.metadata()?.len() == 0 {
            write_all_at(&writer.file, &encode_hint(0), 0)?;
            return Ok(writer);
        }
        writer.seal()?;
        Ok(writer)
    }

    #[cfg(test)]
    pub(super) fn with_frame_logical_bytes(mut self, bytes: u64) -> Self {
        self.frame_logical_bytes = bytes;
        self
    }

    /// Ends whatever a previous writer left open, keeping exactly its complete lines. The bytes
    /// past the kept layout are quarantined before the file is cut.
    fn seal(&mut self) -> io::Result<()> {
        let survey = survey(&self.file)?;
        self.last_trailer = survey.last_trailer;
        self.logical_end = survey.closed_logical_end();
        let cut = match &survey.remainder {
            Remainder::Closed => survey.valid_end,
            Remainder::Untrailed { span, .. } => span.end,
            Remainder::Open { cut, .. } => *cut,
        };
        quarantine(&self.path, &self.file, cut)?;
        let unsealed = match survey.remainder {
            Remainder::Closed => None,
            Remainder::Untrailed { span, .. } => Some(span),
            Remainder::Open {
                start,
                cut,
                logical_start,
                content,
                checksummed,
            } => {
                self.file.set_len(cut)?;
                let mut seal = EMPTY_LAST_BLOCK.to_vec();
                if checksummed {
                    // zstd stores the low 32 bits of the frame content's XXH64.
                    seal.extend_from_slice(&xxh64(&content).to_le_bytes()[..4]);
                }
                write_all_at(&self.file, &seal, cut)?;
                Some(Span {
                    start,
                    end: cut + seal.len() as u64,
                    logical_start,
                    logical_len: content.len() as u64,
                })
            }
        };
        if let Some(span) = unsealed {
            self.file.set_len(span.end)?;
            self.end = span.end;
            self.logical_end = span.logical_end();
            return self.write_trailer(span);
        }
        self.file.set_len(survey.valid_end)?;
        self.end = survey.valid_end;
        if survey.hinted == self.last_trailer {
            return Ok(());
        }
        write_all_at(
            &self.file,
            &self.last_trailer.to_le_bytes(),
            HINT_TRAILER_FIELD,
        )
    }

    /// Appends one newline-terminated line. `flush` ends the current block so the line survives a
    /// crash; bulk re-encoding leaves it off for density.
    pub(crate) fn append(&mut self, line: &[u8], flush: bool) -> io::Result<()> {
        self.ensure_unchanged()?;
        let open = *self.open.get_or_insert(OpenFrame {
            start: self.end,
            logical_start: self.logical_end,
            last_append: Instant::now(),
        });
        let directive = if flush {
            ZSTD_EndDirective::ZSTD_e_flush
        } else {
            ZSTD_EndDirective::ZSTD_e_continue
        };
        self.compress(line, directive)?;
        self.logical_end += line.len() as u64;
        self.open = Some(OpenFrame {
            last_append: Instant::now(),
            ..open
        });
        if self.logical_end - open.logical_start >= self.frame_logical_bytes {
            self.close_frame()?;
        }
        Ok(())
    }

    /// Fails closed when the file's length is not where this writer left it; see the lock model.
    fn ensure_unchanged(&self) -> io::Result<()> {
        let len = self.file.metadata()?.len();
        if len == self.end {
            return Ok(());
        }
        tracing::error!(
            rollout = %self.path.display(),
            expected = self.end,
            found = len,
            "framed rollout changed under its writer; refusing to write"
        );
        Err(io::Error::other(format!(
            "{} changed under its writer: expected {} bytes, found {len}",
            self.path.display(),
            self.end
        )))
    }

    /// Ends the open frame, if any, and records it in a trailer and the hint.
    pub(crate) fn close_frame(&mut self) -> io::Result<()> {
        let Some(open) = self.open else {
            return Ok(());
        };
        self.ensure_unchanged()?;
        self.compress(&[], ZSTD_EndDirective::ZSTD_e_end)?;
        self.open = None;
        self.write_trailer(Span {
            start: open.start,
            end: self.end,
            logical_start: open.logical_start,
            logical_len: self.logical_end - open.logical_start,
        })
    }

    /// When the open frame should close for idleness.
    pub(crate) fn idle_deadline(&self) -> Option<Instant> {
        self.open.map(|open| open.last_append + IDLE_FRAME_CLOSE)
    }

    /// Decoded bytes written so far.
    pub(crate) fn logical_len(&self) -> u64 {
        self.logical_end
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    fn write_trailer(&mut self, span: Span) -> io::Result<()> {
        write_all_at(
            &self.file,
            &encode_trailer(&span, self.last_trailer),
            self.end,
        )?;
        self.last_trailer = self.end;
        self.end += TRAILER_LEN as u64;
        // The hint must never name a trailer that a crash could lose.
        self.file.sync_data()?;
        write_all_at(
            &self.file,
            &self.last_trailer.to_le_bytes(),
            HINT_TRAILER_FIELD,
        )
    }

    fn compress(&mut self, input: &[u8], directive: ZSTD_EndDirective) -> io::Result<()> {
        self.scratch.clear();
        let mut source = InBuffer::around(input);
        loop {
            self.scratch.reserve(CCtx::out_size().max(input.len() / 2));
            let filled = self.scratch.len();
            let remaining = {
                let mut sink = OutBuffer::around_pos(&mut self.scratch, filled);
                self.encoder
                    .compress_stream2(&mut sink, &mut source, directive)
                    .map_err(zstd_error)?
            };
            let done = match directive {
                ZSTD_EndDirective::ZSTD_e_continue => source.pos() == input.len(),
                _ => remaining == 0,
            };
            if done {
                break;
            }
        }
        write_all_at(&self.file, &self.scratch, self.end)?;
        self.end += self.scratch.len() as u64;
        Ok(())
    }
}

/// Writes every line of `input` into the empty `output`, open at `path`, as a framed rollout with
/// every frame closed, terminating an unterminated final line as the plain writer would. The
/// output is synced and then decoded in full; unless it reproduces exactly the bytes fed to the
/// encoder, this fails. Returns the decoded length. Blocking.
pub(crate) fn reencode(mut input: impl BufRead, path: &Path, output: File) -> io::Result<u64> {
    if output.metadata()?.len() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not empty", path.display()),
        ));
    }
    let mut writer = FramedWriter::open(path, output)?;
    let mut fed = Sha256::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            line.push(b'\n');
        }
        fed.update(&line);
        writer.append(&line, /*flush*/ false)?;
    }
    writer.close_frame()?;
    writer.file.sync_all()?;
    let mut reader = FramedReader::open(writer.file.try_clone()?)?;
    let mut decoded = Sha256::new();
    let decoded_len = io::copy(&mut reader, &mut decoded)?;
    if decoded_len != writer.logical_end || decoded.finalize() != fed.finalize() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} does not decode to the bytes encoded into it",
                path.display()
            ),
        ));
    }
    Ok(writer.logical_end)
}
