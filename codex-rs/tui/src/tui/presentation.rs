//! Central admission for display work. Attention never suspends model or server execution.

use super::PresentationMode;
use super::Tui;
use super::frame_requester::Due;
use super::glint::FULL_FRAME_FLOOR;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DesktopAttention {
    pub(crate) idle: bool,
    /// `None` is unknown and never proof that a window is hidden.
    pub(crate) visible: Option<bool>,
    /// `None` is unknown and never proof that a window is unfocused.
    pub(crate) focused: Option<bool>,
}

/// What terminal input proves about the window it reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Witness {
    /// Keys, pastes, focus and resumption: the window is shown and holds the keyboard.
    Keyboard,
    /// The pointer, which crosses windows whether or not they hold the keyboard.
    Pointer,
}

/// What a scheduled draw paints in the current presentation mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScheduledPaint {
    /// The mode admits nothing the screen owes.
    Skip,
    /// Only the status row moved.
    Glint,
    Full,
}

impl Tui {
    pub(crate) fn presentation_mode(&self) -> PresentationMode {
        self.frame_requester.mode()
    }

    pub(crate) fn set_desktop_attention(&mut self, attention: DesktopAttention) {
        self.desktop_idle = attention.idle;
        self.window_visible = attention.visible;
        self.window_focused = attention.focused;
        self.reconcile_presentation();
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn desktop_attention_receiver(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<DesktopAttention>> {
        self.desktop_attention.clone()
    }

    pub(crate) fn reconcile_presentation(&self) {
        let mode = if self.presentation_over_ssh {
            PresentationMode::Reduced
        } else if self.desktop_idle || self.window_visible == Some(false) {
            PresentationMode::Frozen
        } else if self.is_terminal_focused() && self.window_focused != Some(false) {
            PresentationMode::Normal
        } else if self.window_visible == Some(true) {
            PresentationMode::Peripheral
        } else {
            PresentationMode::Reduced
        };
        self.frame_requester.set_mode(mode);
    }

    pub(crate) fn note_terminal_input(&mut self, witness: Witness) {
        // Input proves the observer's hidden or idle report stale, and keyboard input its unfocused
        // one too; a pointer crosses unfocused windows. Never block the user's input on a desktop
        // query or await the next i3 event.
        let hidden = self.window_visible == Some(false);
        let unfocused = witness == Witness::Keyboard && self.window_focused == Some(false);
        if self.desktop_idle || hidden || unfocused {
            self.desktop_idle = false;
            if hidden {
                self.window_visible = Some(true);
            }
            if unfocused {
                self.window_focused = None;
            }
            self.reconcile_presentation();
        }
    }

    pub(crate) fn scheduled_paint(&self) -> ScheduledPaint {
        let owed = self.frame_requester.owed();
        match self.presentation_mode() {
            // A focused window still paints fully each floor, so clocks beyond the status row
            // keep time.
            PresentationMode::Normal
                if owed == Due::GLINT && self.painted_fully_within(FULL_FRAME_FLOOR) =>
            {
                ScheduledPaint::Glint
            }
            PresentationMode::Normal => ScheduledPaint::Full,
            PresentationMode::Peripheral | PresentationMode::Reduced
                if owed.contains(Due::CONTENT) =>
            {
                ScheduledPaint::Full
            }
            PresentationMode::Peripheral if owed.contains(Due::GLINT) => ScheduledPaint::Glint,
            PresentationMode::Peripheral | PresentationMode::Reduced | PresentationMode::Frozen => {
                ScheduledPaint::Skip
            }
        }
    }
}
