//! Logical reads and seeks over a framed rollout, decoding one frame at a time.

use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;

use super::read_exact_at;
use super::survey::Remainder;
use super::survey::Span;
use super::survey::survey;
use super::to_index;

/// A snapshot of a framed rollout as of opening: its frame index is walked once, and the frame
/// last read stays decoded, so repeated and backward reads decode each frame at most once in turn.
pub(crate) struct FramedReader {
    file: File,
    pub(super) spans: Vec<Span>,
    /// Complete lines after the last trailed frame: the open frame's, or an untrailed frame's.
    pub(super) tail: Vec<u8>,
    pub(super) tail_start: u64,
    position: u64,
    decoded: Option<(usize, Vec<u8>)>,
}

impl FramedReader {
    pub(crate) fn open(file: File) -> io::Result<Self> {
        let survey = survey(&file)?;
        let closed_end = survey.closed_logical_end();
        let (tail_start, tail) = match survey.remainder {
            Remainder::Closed => (closed_end, Vec::new()),
            Remainder::Untrailed { span, content } => (span.logical_start, content),
            Remainder::Open {
                logical_start,
                content,
                ..
            } => (logical_start, content),
        };
        Ok(Self {
            file,
            spans: survey.spans,
            tail,
            tail_start,
            position: 0,
            decoded: None,
        })
    }

    /// Decoded length of the snapshot.
    pub(crate) fn logical_len(&self) -> u64 {
        self.tail_start + self.tail.len() as u64
    }

    /// The decoded bytes from `position` to the end of the piece containing it.
    fn piece(&mut self, position: u64) -> io::Result<&[u8]> {
        if position >= self.tail_start {
            return Ok(&self.tail[to_index(position - self.tail_start)?..]);
        }
        let index = self
            .spans
            .partition_point(|span| span.logical_end() <= position);
        let span = self.spans[index];
        if self
            .decoded
            .as_ref()
            .is_none_or(|(cached, _)| *cached != index)
        {
            let mut compressed = vec![0; to_index(span.end - span.start)?];
            read_exact_at(&self.file, &mut compressed, span.start)?;
            let content = zstd::bulk::decompress(&compressed, to_index(span.logical_len)?)?;
            if content.len() as u64 != span.logical_len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "rollout frame decoded to an unexpected length",
                ));
            }
            self.decoded = Some((index, content));
        }
        let content = self
            .decoded
            .as_ref()
            .map_or(&[][..], |(_, content)| content);
        Ok(&content[to_index(position - span.logical_start)?..])
    }
}

impl Read for FramedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.logical_len() || buffer.is_empty() {
            return Ok(0);
        }
        let position = self.position;
        let piece = self.piece(position)?;
        let count = piece.len().min(buffer.len());
        buffer[..count].copy_from_slice(&piece[..count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for FramedReader {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        let (base, delta) = match target {
            SeekFrom::Start(offset) => {
                self.position = offset;
                return Ok(offset);
            }
            SeekFrom::End(delta) => (self.logical_len(), delta),
            SeekFrom::Current(delta) => (self.position, delta),
        };
        self.position = base.checked_add_signed(delta).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "seek before rollout start")
        })?;
        Ok(self.position)
    }
}
