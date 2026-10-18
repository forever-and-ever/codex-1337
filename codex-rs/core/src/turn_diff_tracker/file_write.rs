use super::AppliedPatchChange;
use super::TurnDiffTracker;

impl TurnDiffTracker {
    pub(crate) fn track_file_write(&mut self, environment_id: &str, change: &AppliedPatchChange) {
        if self.valid {
            self.apply_change(environment_id, change);
            self.refresh_unified_diff();
        }
    }
}
