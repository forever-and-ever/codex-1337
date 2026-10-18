//! Cloning freezes the mutable live-cell revision for background rendering.

use super::Arc;
use super::AtomicBool;
use super::Ordering;
use super::SessionHeaderHistoryCell;

impl Clone for SessionHeaderHistoryCell {
    fn clone(&self) -> Self {
        Self {
            version: self.version,
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            directory: self.directory.clone(),
            yolo_mode: self.yolo_mode,
            // Freeze the active banner's independently mutable lifecycle bit.
            fresh_session: Arc::new(AtomicBool::new(self.fresh_session.load(Ordering::Relaxed))),
        }
    }
}
