//! Text-enabled children share draft recall with root conversations, including cleared drafts.

use super::ChatComposer;

impl ChatComposer {
    pub(super) fn history_recall_available(&self) -> bool {
        !self.blocks_direct_input || self.child_text_input_enabled
    }
}
