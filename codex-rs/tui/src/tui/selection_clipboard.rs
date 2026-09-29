//! Submit selection copies without waiting for native clipboard or helper I/O.

use super::Tui;
use crate::clipboard_copy::CopyFormat;
use crate::clipboard_copy::CopyStatus;

impl Tui {
    pub(crate) fn copy_mouse_selection(
        &mut self,
        text: &str,
        format: CopyFormat,
    ) -> Result<CopyStatus, String> {
        if cfg!(target_os = "linux") {
            self.publish_primary_selection(text)
        } else {
            self.copy_transcript_selection(text, format)
        }
    }

    pub(crate) fn publish_primary_selection(&mut self, text: &str) -> Result<CopyStatus, String> {
        #[cfg(target_os = "linux")]
        if !crate::clipboard_copy::is_ssh_session()
            && !crate::clipboard_copy::is_tmux_session()
            && !crate::clipboard_copy::is_wsl_session()
        {
            return self
                .clipboard
                .copy_primary(text.into(), self.frame_requester());
        }
        crate::clipboard_copy::publish_primary(text, self.terminal.backend_mut())
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
