//! Submit selection copies without waiting for native clipboard or helper I/O.

use super::Tui;
use crate::clipboard_copy::CopyFormat;
use crate::clipboard_copy::CopyStatus;
use crate::clipboard_copy::SelectionTarget;
use crate::clipboard_copy::worker::CopyResult;
use std::sync::Arc;

/// Explicit copying owns CLIPBOARD; selecting text owns only PRIMARY.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SelectionCopy {
    Explicit,
    OnSelect { clear_selection: bool },
}

impl SelectionCopy {
    pub(crate) fn copies_clipboard(self) -> bool {
        matches!(self, Self::Explicit)
    }

    pub(crate) fn clear_selection(self) -> bool {
        match self {
            Self::Explicit => true,
            Self::OnSelect { clear_selection } => clear_selection,
        }
    }
}

impl Tui {
    pub(crate) fn copy_selection(
        &mut self,
        text: &str,
        format: CopyFormat,
        selection_copy: SelectionCopy,
    ) -> (CopyResult, Option<Arc<()>>) {
        match selection_copy {
            SelectionCopy::Explicit => (
                self.clipboard
                    .copy(text.into(), format, self.frame_requester()),
                None,
            ),
            SelectionCopy::OnSelect { .. } => {
                let (result, owner) = self.clipboard.select(
                    text.into(),
                    CopyFormat::PlainText,
                    SelectionTarget::Primary,
                    self.frame_requester(),
                );
                (result, Some(owner))
            }
        }
    }

    pub(crate) fn publish_primary_selection(&mut self, text: &str) -> Result<CopyStatus, String> {
        let (result, owner) = self.copy_selection(
            text,
            CopyFormat::PlainText,
            SelectionCopy::OnSelect {
                clear_selection: false,
            },
        );
        self.selection_clipboard_owner = owner;
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
        self.copy_selection(text, format, SelectionCopy::Explicit).0
    }
}
