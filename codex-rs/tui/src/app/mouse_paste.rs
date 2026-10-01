//! Pointer paste into an unchanged, editable fullscreen composer: CLIPBOARD on configured
//! right-click, PRIMARY on Linux middle-click, without changing the terminal's shortcuts.
//! Existing selection handlers run first. Only a draw may deliver a read, so real input
//! invalidates pending paste before completion and is never replaced by clipboard text.

use super::*;
use crate::clipboard_copy::SelectionTarget;
use crate::tui::VscodeDetection;
use codex_config::types::RightClickPaste;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;

pub(super) struct PendingPaste {
    source: SelectionTarget,
    thread: Option<ThreadId>,
    draft: (String, usize),
}

pub(super) struct PasteEnvironment {
    pub(super) platform_default: bool,
    pub(super) ssh: bool,
    pub(super) wsl: bool,
    pub(super) vscode: VscodeDetection,
}

impl PasteEnvironment {
    pub(super) fn detect() -> Self {
        Self {
            platform_default: cfg!(any(target_os = "windows", target_os = "linux")),
            ssh: crate::clipboard_copy::is_ssh_session(),
            wsl: crate::clipboard_copy::is_wsl_session(),
            vscode: tui::detect_vscode_terminal(),
        }
    }

    fn allows(&self, source: SelectionTarget, mode: RightClickPaste) -> bool {
        if self.ssh || self.vscode == VscodeDetection::VsCode {
            return false;
        }
        if source == SelectionTarget::Primary {
            return cfg!(target_os = "linux") && !self.wsl;
        }
        match mode {
            RightClickPaste::Off => false,
            RightClickPaste::On => !cfg!(target_os = "android"),
            RightClickPaste::Auto => {
                self.platform_default && !(self.wsl && self.vscode == VscodeDetection::Unknown)
            }
        }
    }
}

impl App {
    fn mouse_paste_target(&self, tui: &tui::Tui, source: SelectionTarget) -> Option<PendingPaste> {
        if !tui.is_owned_screen()
            || self.overlay.is_some()
            || (source == SelectionTarget::Clipboard && self.transcript_view.has_selection_range())
            || self.transcript_view.is_search_active()
            || !self
                .mouse_paste_environment
                .allows(source, self.local_settings.tui.right_click_paste)
        {
            return None;
        }
        Some(PendingPaste {
            source,
            thread: self.current_displayed_thread_id(),
            draft: self.chat_widget.mouse_paste_target(source)?,
        })
    }

    pub(super) fn start_mouse_paste(&mut self, tui: &mut tui::Tui, mouse: MouseEvent) {
        let source = match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => SelectionTarget::Clipboard,
            MouseEventKind::Down(MouseButton::Middle) => SelectionTarget::Primary,
            _ => return,
        };
        if !mouse.modifiers.is_empty()
            || self.pending_mouse_paste.is_some()
            || tui.clipboard.is_busy()
        {
            return;
        }
        let Some(target) = self.mouse_paste_target(tui, source) else {
            return;
        };
        match tui.clipboard.read_text(source, tui.frame_requester()) {
            Ok(true) => self.pending_mouse_paste = Some(target),
            Ok(false) => {}
            Err(error) => self.chat_widget.add_error_message(error),
        }
    }

    /// Invalidate before polling, even when the worker has already completed.
    pub(super) fn invalidate_mouse_paste(&mut self, event: &TuiEvent) {
        let keep = match event {
            TuiEvent::Draw | TuiEvent::Resize(_) | TuiEvent::FocusGained => true,
            TuiEvent::Mouse(mouse) => {
                matches!(
                    mouse.kind,
                    MouseEventKind::Moved
                        | MouseEventKind::Up(MouseButton::Right | MouseButton::Middle)
                ) || (matches!(
                    mouse.kind,
                    MouseEventKind::Down(MouseButton::Right | MouseButton::Middle)
                ) && mouse.modifiers.is_empty())
            }
            TuiEvent::Key(_) | TuiEvent::Paste(_) | TuiEvent::FocusLost | TuiEvent::Resume => false,
        };
        if !keep {
            self.pending_mouse_paste = None;
        }
    }

    pub(super) fn finish_mouse_paste(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> TuiEvent {
        if !matches!(event, TuiEvent::Draw) {
            return event;
        }
        let Some(result) = tui.clipboard.take_text_result() else {
            return event;
        };
        let Some(pending) = self.pending_mouse_paste.take() else {
            return event;
        };
        let Some(current) = self.mouse_paste_target(tui, pending.source) else {
            return event;
        };
        if pending.thread != current.thread || pending.draft != current.draft {
            return event;
        }
        match result {
            Ok(text) if !text.is_empty() => {
                tui.frame_requester().schedule_frame();
                TuiEvent::Paste(text)
            }
            Ok(_) => event,
            Err(error) => {
                self.chat_widget.add_error_message(error);
                event
            }
        }
    }
}

#[cfg(test)]
#[path = "mouse_paste_tests.rs"]
mod tests;
