//! Capability observations returned by owned operation workers, committed by the session owner.

use super::AppServerSession;

impl AppServerSession {
    pub(crate) fn operation_settings_supported(&self) -> bool {
        self.thread_settings_update_supported
    }

    pub(crate) fn note_operation_settings_unsupported(&mut self) {
        self.thread_settings_update_supported = false;
    }
}
