//! Central admission for display work. Attention never suspends model or server execution.

use super::PresentationMode;
use super::Tui;

impl Tui {
    pub(crate) fn presentation_mode(&self) -> PresentationMode {
        self.frame_requester.mode()
    }

    pub(crate) fn set_desktop_attention(&mut self, idle: bool, visible: Option<bool>) {
        self.desktop_idle = idle;
        self.window_visible = visible;
        self.reconcile_presentation();
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn desktop_attention_receiver(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<super::desktop_attention::DesktopAttention>> {
        self.desktop_attention.clone()
    }

    pub(crate) fn reconcile_presentation(&self) {
        let mode = if self.presentation_over_ssh {
            PresentationMode::Reduced
        } else if self.desktop_idle || self.window_visible == Some(false) {
            PresentationMode::Frozen
        } else if self.is_terminal_focused() {
            PresentationMode::Normal
        } else {
            PresentationMode::Reduced
        };
        self.frame_requester.set_mode(mode);
    }

    pub(crate) fn note_terminal_input(&mut self) {
        // Terminal input or focus proves the observer's old hidden/idle report is stale.
        // Never block the user's input on a desktop query or await the next i3 event.
        if self.desktop_idle || self.window_visible == Some(false) {
            self.desktop_idle = false;
            self.window_visible = None;
            self.reconcile_presentation();
        }
    }

    pub(crate) fn should_render_scheduled_frame(&self) -> bool {
        match self.presentation_mode() {
            PresentationMode::Normal => true,
            PresentationMode::Reduced => self.frame_requester.take_dirty(),
            PresentationMode::Frozen => false,
        }
    }
}
