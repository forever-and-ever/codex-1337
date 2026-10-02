//! Submit selection copies without waiting for native clipboard or helper I/O.

use super::Tui;
use crate::clipboard_copy::CopyFormat;
use crate::clipboard_copy::CopyStatus;
use crate::clipboard_copy::SelectionTarget;

impl Tui {
    pub(crate) fn publish_primary_selection(&mut self, text: &str) -> Result<CopyStatus, String> {
        let (result, owner) = self.clipboard.select(
            text.into(),
            CopyFormat::PlainText,
            SelectionTarget::Primary,
            self.frame_requester(),
        );
        self.selection_clipboard_owner = Some(owner);
        result
    }

    pub(crate) fn cancel_mouse_selection(&mut self) {
        self.selection_clipboard_owner = None;
    }

    pub(crate) fn copy_transcript_selection(
        &mut self,
        text: &str,
        format: CopyFormat,
    ) -> Result<CopyStatus, String> {
        self.clipboard
            .copy(text.into(), format, self.frame_requester())
    }
}
