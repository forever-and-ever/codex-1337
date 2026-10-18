//! Preserve record bytes when canonical reconciliation needs a physical retry witness.

use std::io;
use std::io::Read;
use std::io::Seek;

use super::ReverseJsonlScanner;
use super::ScanOutcome;
use crate::RolloutLine;

impl<R: Read + Seek> ReverseJsonlScanner<R> {
    /// Returns the exact nonblank record bytes alongside the ordinary decoder outcome.
    /// The borrowed bytes remain valid until the next scan. Capture is enabled lazily here.
    pub fn scan_next_rollout_line_with_record(
        &mut self,
    ) -> io::Result<Option<(ScanOutcome<RolloutLine>, &[u8])>> {
        let _ = self.record_capture.get_or_insert_default();
        let outcome = self.scan_next_rollout_line()?;
        let Some(record) = self.record_capture.as_deref() else {
            unreachable!("record capture enabled before scanning");
        };
        Ok(outcome.map(|outcome| (outcome, record)))
    }

    pub(super) fn capture_record(&mut self) {
        if let Some(capture) = &mut self.record_capture {
            capture.clear();
            capture.extend_from_slice(&self.record_reversed);
        }
    }
}
