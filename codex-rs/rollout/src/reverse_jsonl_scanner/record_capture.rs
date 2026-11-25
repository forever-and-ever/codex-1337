//! Preserve record bytes when canonical reconciliation needs a physical retry witness.

use std::io;
use std::io::Read;
use std::io::Seek;

use super::ReverseJsonlScanner;
use super::ScanOutcome;
use crate::RolloutLine;

#[derive(Default)]
pub(super) struct RecordCapture {
    bytes: Vec<u8>,
    offset: u64,
}

impl<R: Read + Seek> ReverseJsonlScanner<R> {
    /// Reverse scan a record-aligned logical suffix without reading the validated prefix.
    pub fn new_range(reader: R, start: u64, end: u64) -> io::Result<Self> {
        if start > end {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reverse JSONL scan start is past its end",
            ));
        }
        let mut scanner = Self::new_at(reader, end)?;
        scanner.min_byte_offset = start;
        Ok(scanner)
    }

    /// Returns the nonblank record's outcome, exact bytes and starting logical byte offset.
    pub fn scan_next_rollout_line_with_record(
        &mut self,
    ) -> io::Result<Option<(ScanOutcome<RolloutLine>, &[u8], u64)>> {
        self.record_capture.get_or_insert_default();
        let outcome = self.scan_next_rollout_line()?;
        let Some(record) = self.record_capture.as_ref() else {
            unreachable!("record capture enabled before scanning");
        };
        Ok(outcome.map(|outcome| (outcome, record.bytes.as_slice(), record.offset)))
    }

    pub(super) fn capture_record(&mut self, offset: u64) {
        if let Some(capture) = &mut self.record_capture {
            capture.bytes.clear();
            capture.bytes.extend_from_slice(&self.record_reversed);
            capture.offset = offset;
        }
    }
}
