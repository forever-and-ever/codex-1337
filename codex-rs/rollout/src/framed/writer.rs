//! Appends lines to a framed rollout, closing frames by size or idleness and sealing on open.

use std::fs::File;
use std::io;
use std::io::BufRead;
use std::time::Instant;

use zstd::zstd_safe::CCtx;
use zstd::zstd_safe::CParameter;
use zstd::zstd_safe::InBuffer;
use zstd::zstd_safe::OutBuffer;
use zstd::zstd_safe::ResetDirective;
use zstd::zstd_safe::zstd_sys::ZSTD_EndDirective;

use super::COMPRESSION_LEVEL;
use super::FRAME_LOGICAL_BYTES;
use super::HINT_LEN;
use super::HINT_TRAILER_FIELD;
use super::IDLE_FRAME_CLOSE;
use super::TRAILER_LEN;
use super::WINDOW_LOG;
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
    /// Takes `file` (readable and writable, not in append mode) for appending: an empty file
    /// receives the hint, and a framed one has its open frame sealed. The caller holds the
    /// rollout's writer lock.
    pub(crate) fn open(file: File) -> io::Result<Self> {
        let mut encoder = CCtx::create();
        for parameter in [
            CParameter::CompressionLevel(COMPRESSION_LEVEL),
            CParameter::WindowLog(WINDOW_LOG),
            CParameter::ChecksumFlag(true),
        ] {
            encoder.set_parameter(parameter).map_err(zstd_error)?;
        }
        let mut writer = Self {
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

    /// Ends whatever a previous writer left open, keeping exactly its complete lines.
    fn seal(&mut self) -> io::Result<()> {
        let survey = survey(&self.file)?;
        self.last_trailer = survey.last_trailer;
        self.logical_end = survey.closed_logical_end();
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
        if self.file.metadata()?.len() != self.end {
            self.resync()?;
        }
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

    /// Another writer sealed this file and appended (an unloaded thread's metadata update), so
    /// this writer's frame is no longer open: start over from the file's current layout.
    fn resync(&mut self) -> io::Result<()> {
        self.encoder
            .reset(ResetDirective::SessionOnly)
            .map_err(zstd_error)?;
        self.open = None;
        self.seal()
    }

    /// Ends the open frame, if any, and records it in a trailer and the hint.
    pub(crate) fn close_frame(&mut self) -> io::Result<()> {
        let Some(open) = self.open else {
            return Ok(());
        };
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
    #[cfg(test)]
    pub(super) fn logical_len(&self) -> u64 {
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

/// Writes every line of `input` into the empty `output` as a framed rollout with every frame
/// closed, terminating an unterminated final line as the plain writer would. Returns the decoded
/// length.
pub(crate) fn reencode(mut input: impl BufRead, output: File) -> io::Result<u64> {
    let mut writer = FramedWriter::open(output)?;
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            line.push(b'\n');
        }
        writer.append(&line, /*flush*/ false)?;
    }
    writer.close_frame()?;
    writer.file.sync_all()?;
    Ok(writer.logical_end)
}
