//! Reads a framed rollout's layout: the trailer chain the hint names, then whatever follows it.

use std::fs::File;
use std::io;

use zstd::zstd_safe::DCtx;
use zstd::zstd_safe::InBuffer;
use zstd::zstd_safe::OutBuffer;

use super::FORMAT_TAG;
use super::FRAME_MAGIC;
use super::HINT_BODY_LEN;
use super::HINT_LEN;
use super::HINT_MAGIC;
use super::TRAILER_BODY_LEN;
use super::TRAILER_LEN;
use super::TRAILER_MAGIC;
use super::read_exact_at;
use super::to_index;
use super::u32_le;
use super::u64_le;

/// One closed frame: its compressed extent and the logical bytes it decodes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) start: u64,
    /// Just past the frame: the offset of its trailer.
    pub(super) end: u64,
    pub(super) logical_start: u64,
    pub(super) logical_len: u64,
}

impl Span {
    pub(super) fn logical_end(&self) -> u64 {
        self.logical_start + self.logical_len
    }
}

/// What follows the newest valid trailer.
pub(super) enum Remainder {
    Closed,
    /// A complete frame whose trailer was never written.
    Untrailed {
        span: Span,
        content: Vec<u8>,
    },
    /// The frame being appended, cut to its last complete line, which it decodes to `content`.
    Open {
        start: u64,
        cut: u64,
        logical_start: u64,
        content: Vec<u8>,
        checksummed: bool,
    },
}

pub(super) struct Survey {
    /// Trailed frames, oldest first.
    pub(super) spans: Vec<Span>,
    /// Offset of the newest valid trailer, or 0 when none exists.
    pub(super) last_trailer: u64,
    /// The trailer the hint names, which may be stale.
    pub(super) hinted: u64,
    pub(super) remainder: Remainder,
    /// Bytes from here on are torn or foreign.
    pub(super) valid_end: u64,
}

impl Survey {
    pub(super) fn closed_logical_end(&self) -> u64 {
        self.spans.last().map_or(0, Span::logical_end)
    }
}

pub(super) fn encode_hint(last_trailer: u64) -> [u8; HINT_LEN] {
    let mut hint = [0; HINT_LEN];
    hint[..4].copy_from_slice(&HINT_MAGIC.to_le_bytes());
    hint[4..8].copy_from_slice(&HINT_BODY_LEN.to_le_bytes());
    hint[8..12].copy_from_slice(&FORMAT_TAG);
    hint[16..].copy_from_slice(&last_trailer.to_le_bytes());
    hint
}

pub(super) fn encode_trailer(span: &Span, previous: u64) -> [u8; TRAILER_LEN] {
    let mut trailer = [0; TRAILER_LEN];
    trailer[..4].copy_from_slice(&TRAILER_MAGIC.to_le_bytes());
    trailer[4..8].copy_from_slice(&TRAILER_BODY_LEN.to_le_bytes());
    trailer[8..16].copy_from_slice(&span.start.to_le_bytes());
    trailer[16..24].copy_from_slice(&span.logical_start.to_le_bytes());
    trailer[24..32].copy_from_slice(&span.logical_len.to_le_bytes());
    trailer[32..].copy_from_slice(&previous.to_le_bytes());
    trailer
}

/// The frame span and previous-trailer offset recorded by a trailer at `offset`.
fn decode_trailer(trailer: &[u8], offset: u64) -> Option<(Span, u64)> {
    (u32_le(trailer) == TRAILER_MAGIC && u32_le(&trailer[4..]) == TRAILER_BODY_LEN).then(|| {
        let span = Span {
            start: u64_le(&trailer[8..]),
            end: offset,
            logical_start: u64_le(&trailer[16..]),
            logical_len: u64_le(&trailer[24..]),
        };
        (span, u64_le(&trailer[32..]))
    })
}

fn read_hint(file: &File) -> io::Result<(u64, u64)> {
    let len = file.metadata()?.len();
    let mut hint = [0; HINT_LEN];
    read_exact_at(file, &mut hint, 0)?;
    if u32_le(&hint) != HINT_MAGIC || hint[8..12] != FORMAT_TAG {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a framed rollout",
        ));
    }
    Ok((len, u64_le(&hint[16..])))
}

/// Reads and verifies the first frame without surveying subsequent frames or the open tail.
/// A nonzero index hint and an unverified first frame require the normal reader's recovery rules.
pub(crate) fn read_first_frame(file: &File) -> io::Result<Option<Vec<u8>>> {
    let (len, hinted) = read_hint(file)?;
    let start = HINT_LEN as u64;
    let mut source = Window::new(file, len);
    if source
        .slice(start, 4)?
        .is_none_or(|magic| u32_le(magic) != FRAME_MAGIC)
    {
        return Ok((hinted == 0).then(Vec::new));
    }
    match scan_frame(&mut source, start)? {
        Scanned::Complete { content, .. } => Ok(Some(content)),
        Scanned::Open { content, .. } => Ok((hinted == 0).then_some(content)),
    }
}

pub(super) fn survey(file: &File) -> io::Result<Survey> {
    let (len, hinted) = read_hint(file)?;
    // A chain that fails verification is ignored; walking from the start recovers the layout.
    let (mut spans, mut last_trailer) = chain(file, hinted, len)?.unwrap_or_default();
    let mut logical = spans.last().map_or(0, Span::logical_end);
    let mut at = if last_trailer == 0 {
        HINT_LEN as u64
    } else {
        last_trailer + TRAILER_LEN as u64
    };
    let mut source = Window::new(file, len);
    let (remainder, valid_end) = loop {
        if source
            .slice(at, 4)?
            .is_none_or(|magic| u32_le(magic) != FRAME_MAGIC)
        {
            break (Remainder::Closed, at.min(len));
        }
        match scan_frame(&mut source, at)? {
            Scanned::Complete { end, content } => {
                let span = Span {
                    start: at,
                    end,
                    logical_start: logical,
                    logical_len: content.len() as u64,
                };
                let trailed = source
                    .slice(end, TRAILER_LEN as u64)?
                    .and_then(|trailer| decode_trailer(trailer, end));
                if trailed == Some((span, last_trailer)) {
                    spans.push(span);
                    last_trailer = end;
                    logical = span.logical_end();
                    at = end + TRAILER_LEN as u64;
                    continue;
                }
                break (Remainder::Untrailed { span, content }, end);
            }
            Scanned::Open {
                cut,
                content,
                checksummed,
            } if !content.is_empty() => {
                break (
                    Remainder::Open {
                        start: at,
                        cut,
                        logical_start: logical,
                        content,
                        checksummed,
                    },
                    cut,
                );
            }
            Scanned::Open { .. } => break (Remainder::Closed, at),
        }
    };
    Ok(Survey {
        spans,
        last_trailer,
        hinted,
        remainder,
        valid_end,
    })
}

/// The trailed frames the hint names, oldest first, if the whole chain verifies.
fn chain(file: &File, hinted: u64, len: u64) -> io::Result<Option<(Vec<Span>, u64)>> {
    let mut spans = Vec::new();
    let mut at = hinted;
    while at != 0 {
        if at
            .checked_add(TRAILER_LEN as u64)
            .is_none_or(|end| end > len)
        {
            return Ok(None);
        }
        let mut trailer = [0; TRAILER_LEN];
        read_exact_at(file, &mut trailer, at)?;
        let Some((span, previous)) = decode_trailer(&trailer, at) else {
            return Ok(None);
        };
        let frame_follows = if previous == 0 {
            span.start == HINT_LEN as u64
        } else {
            previous.checked_add(TRAILER_LEN as u64) == Some(span.start)
        };
        if !frame_follows || span.start >= at {
            return Ok(None);
        }
        spans.push(span);
        at = previous;
    }
    spans.reverse();
    let mut logical = 0;
    for span in &spans {
        if span.logical_start != logical {
            return Ok(None);
        }
        logical = span.logical_end();
    }
    Ok(Some((spans, hinted)))
}

enum Scanned {
    Complete {
        end: u64,
        content: Vec<u8>,
    },
    /// Incomplete or damaged. `content` holds the complete lines that end at a block
    /// boundary, and `cut` is that boundary.
    Open {
        cut: u64,
        content: Vec<u8>,
        checksummed: bool,
    },
}

/// Decodes the frame at `start` block by block, so an incomplete frame yields its complete lines.
fn scan_frame(source: &mut Window<'_>, start: u64) -> io::Result<Scanned> {
    let mut content = Vec::new();
    let torn = |content: Vec<u8>, cut: u64, checksummed: bool| Scanned::Open {
        cut,
        content,
        checksummed,
    };
    let Some(descriptor) = source.slice(start, 5)?.map(|head| head[4]) else {
        return Ok(torn(content, start, false));
    };
    let single_segment = descriptor & 0x20 != 0;
    let checksummed = descriptor & 0x04 != 0;
    let dictionary_bytes = [0, 1, 2, 4][usize::from(descriptor & 3)];
    let content_size_bytes = match descriptor >> 6 {
        0 => u64::from(single_segment),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let header_len = 5 + u64::from(!single_segment) + dictionary_bytes + content_size_bytes;
    let mut decoder = DCtx::create();
    let Some(header) = source.slice(start, header_len)? else {
        return Ok(torn(content, start, checksummed));
    };
    if feed(&mut decoder, header, &mut content).is_err() {
        return Ok(torn(content, start, checksummed));
    }
    // The newest block boundary at which the decoded bytes end a line.
    let mut kept = (start, 0);
    let mut at = start + header_len;
    while let Some(block_header) = source.slice(at, 3)? {
        let word = u32::from(block_header[0])
            | u32::from(block_header[1]) << 8
            | u32::from(block_header[2]) << 16;
        let last = word & 1 != 0;
        let payload = match (word >> 1) & 3 {
            0 | 2 => u64::from(word >> 3),
            1 => 1,
            _ => break,
        };
        let Some(block) = source.slice(at, 3 + payload)? else {
            break;
        };
        let Ok(hint) = feed(&mut decoder, block, &mut content) else {
            break;
        };
        at += 3 + payload;
        if last {
            let checksum_len = if checksummed { 4 } else { 0 };
            let complete = if checksummed {
                match source.slice(at, checksum_len)? {
                    Some(checksum) => feed(&mut decoder, checksum, &mut content) == Ok(0),
                    None => false,
                }
            } else {
                hint == 0
            };
            if complete {
                return Ok(Scanned::Complete {
                    end: at + checksum_len,
                    content,
                });
            }
            // A last block cannot be followed by another, so sealing cuts before it.
            break;
        }
        if content.last() == Some(&b'\n') {
            kept = (at, content.len());
        }
    }
    content.truncate(kept.1);
    Ok(torn(content, kept.0, checksummed))
}

/// Feeds `input` to the decoder, appending everything it produces; returns zstd's next-input hint.
fn feed(decoder: &mut DCtx<'_>, input: &[u8], output: &mut Vec<u8>) -> Result<usize, usize> {
    let mut source = InBuffer::around(input);
    loop {
        output.reserve(1 << 18);
        let filled = output.len();
        let capacity = output.capacity();
        let (hint, full) = {
            let mut sink = OutBuffer::around_pos(output, filled);
            let hint = decoder.decompress_stream(&mut sink, &mut source)?;
            (hint, sink.pos() == capacity)
        };
        if source.pos() == input.len() && !full {
            return Ok(hint);
        }
    }
}

/// Positional reads through one reusable buffer.
pub(super) struct Window<'a> {
    file: &'a File,
    len: u64,
    base: u64,
    buffer: Vec<u8>,
}

impl<'a> Window<'a> {
    pub(super) fn new(file: &'a File, len: u64) -> Self {
        Self {
            file,
            len,
            base: 0,
            buffer: Vec::new(),
        }
    }

    /// The `count` bytes at `offset`, or `None` when the file ends first.
    pub(super) fn slice(&mut self, offset: u64, count: u64) -> io::Result<Option<&[u8]>> {
        let Some(end) = offset.checked_add(count).filter(|end| *end <= self.len) else {
            return Ok(None);
        };
        let buffered_end = self.base + self.buffer.len() as u64;
        if offset < self.base || end > buffered_end {
            let fill = (self.len - offset).min(count.max(1 << 20));
            self.buffer.resize(to_index(fill)?, 0);
            read_exact_at(self.file, &mut self.buffer, offset)?;
            self.base = offset;
        }
        let begin = to_index(offset - self.base)?;
        Ok(Some(&self.buffer[begin..begin + to_index(count)?]))
    }
}
