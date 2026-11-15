//! Clipboard text paste from Windows shortcuts, right-click, or X11 middle-click PRIMARY.
//! The policy also reports why a configured mode is unavailable in this terminal.
//! Existing selection handlers run first. Only a draw may deliver a read, so real input
//! invalidates pending paste before completion and is never replaced by clipboard text.
//! Releasing or pressing another paste shortcut does not cancel its pending paste.

use super::App;
use super::Arc;
use super::ThreadId;
use super::TuiEvent;
use super::tui;
use crate::chatwidget::ClipboardPasteContext;
use crate::clipboard_copy::SelectionTarget;
use crate::transcript_mode::TranscriptMode;
use crate::tui::VscodeDetection;
use codex_config::types::RightClickPaste;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;

pub(super) struct PendingPaste {
    target: (Option<ThreadId>, crate::bottom_pane::ClipboardPasteTarget),
    source: SelectionTarget,
    shortcut: Option<KeyCode>,
    // Retain the clipboard request until completion or explicit cancellation.
    pub(super) _request: Arc<()>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "These are independent host capabilities and environment observations, not mutually exclusive states."
)]
pub(crate) struct PasteEnvironment {
    pub(super) primary: bool,
    pub(super) platform_default: bool,
    pub(super) ssh: bool,
    pub(super) wsl: bool,
    pub(super) vscode: VscodeDetection,
}

impl PasteEnvironment {
    pub(crate) fn detect() -> Self {
        Self {
            primary: crate::clipboard_copy::primary::available(),
            platform_default: cfg!(any(target_os = "windows", target_os = "linux")),
            ssh: crate::clipboard_copy::is_ssh_session(),
            wsl: crate::clipboard_copy::is_wsl_session(),
            vscode: tui::detect_vscode_terminal(),
        }
    }

    pub(super) fn allows(&self, mode: RightClickPaste, transcript_mode: TranscriptMode) -> bool {
        self.status(mode, transcript_mode)
            .unwrap_or(/*default*/ false)
    }

    /// Return the effective state or why this terminal must handle paste itself.
    pub(crate) fn status(
        &self,
        mode: RightClickPaste,
        transcript_mode: TranscriptMode,
    ) -> Result<bool, &'static str> {
        if mode == RightClickPaste::Off {
            return Ok(false);
        }
        if !transcript_mode.is_owned() {
            return Err("Unavailable in inline mode; use your terminal's paste command");
        }
        if self.ssh {
            return Err("Unavailable over SSH; use your terminal's paste command");
        }
        if self.vscode == VscodeDetection::VsCode {
            return Err("VS Code handles pasting; use its terminal paste command");
        }
        if mode == RightClickPaste::Auto && self.wsl && self.vscode == VscodeDetection::Unknown {
            return Err(
                "Auto cannot identify this WSL terminal; use On or your terminal's paste command",
            );
        }
        match mode {
            RightClickPaste::Off => Ok(false),
            RightClickPaste::On if cfg!(target_os = "android") => Err("Unavailable on Android"),
            RightClickPaste::On => Ok(true),
            RightClickPaste::Auto => Ok(self.platform_default),
        }
    }
}

impl App {
    pub(super) fn clipboard_paste_target(
        &self,
        tui: &tui::Tui,
        source: SelectionTarget,
        shortcut: Option<KeyCode>,
    ) -> Option<(Option<ThreadId>, crate::bottom_pane::ClipboardPasteTarget)> {
        let allowed = shortcut.is_some()
            || match source {
                SelectionTarget::Clipboard => {
                    !self.transcript_view.has_selection_range()
                        && !self
                            .chat_widget
                            .rendered_selection
                            .borrow()
                            .view
                            .has_selection_range()
                        && self.mouse_paste_environment.allows(
                            self.local_settings.tui.right_click_paste,
                            self.local_settings.transcript_mode,
                        )
                }
                SelectionTarget::Primary => {
                    self.mouse_paste_environment.primary
                        && !self.mouse_paste_environment.ssh
                        && self.mouse_paste_environment.vscode != VscodeDetection::VsCode
                }
            };
        if (shortcut.is_none() && !tui.is_owned_screen())
            || self.overlay.is_some()
            || self.transcript_view.is_search_editing()
            || !allowed
        {
            return None;
        }
        Some((
            self.current_displayed_thread_id(),
            self.chat_widget.clipboard_paste_target(source, shortcut)?,
        ))
    }

    pub(super) fn start_mouse_paste(&mut self, tui: &mut tui::Tui, mouse: MouseEvent) {
        self.start_mouse_paste_in(tui, mouse, ClipboardPasteContext::Composer);
    }

    pub(super) fn start_mouse_paste_in(
        &mut self,
        tui: &mut tui::Tui,
        mouse: MouseEvent,
        context: ClipboardPasteContext,
    ) {
        let source = match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => SelectionTarget::Clipboard,
            MouseEventKind::Down(MouseButton::Middle) => SelectionTarget::Primary,
            _ => return,
        };
        if !mouse.modifiers.is_empty() || self.pending_mouse_paste.is_some() {
            return;
        }
        // The restricted handoff does not run the ordinary mouse editor first. Settle any
        // held typing before taking the target of an eligible click.
        if context == ClipboardPasteContext::DraftHandoff
            && (self
                .clipboard_paste_target(tui, source, /*shortcut*/ None)
                .is_none()
                || !self.chat_widget.prepare_composer_mouse(mouse))
        {
            return;
        }
        self.start_clipboard_text_paste(tui, source, /*shortcut*/ None);
    }

    pub(crate) fn start_clipboard_text_paste(
        &mut self,
        tui: &mut tui::Tui,
        source: SelectionTarget,
        shortcut: Option<KeyCode>,
    ) {
        let Some(target) = self.clipboard_paste_target(tui, source, shortcut) else {
            return;
        };
        match tui.clipboard.read_text(source, tui.frame_requester()) {
            Ok(Some(request)) => {
                self.pending_mouse_paste = Some(PendingPaste {
                    target,
                    source,
                    shortcut,
                    _request: request,
                });
                tui.frame_requester().schedule_frame();
            }
            Ok(None) => {}
            Err(error) => self.chat_widget.add_error_message(error),
        }
    }

    /// Invalidate before polling, even when the worker has already completed.
    pub(super) fn invalidate_mouse_paste(&mut self, event: &TuiEvent) {
        let Some(pending) = &self.pending_mouse_paste else {
            return;
        };
        let button = match pending.source {
            SelectionTarget::Clipboard => MouseButton::Right,
            SelectionTarget::Primary => MouseButton::Middle,
        };
        let keep = match event {
            TuiEvent::Draw | TuiEvent::Resize(_) | TuiEvent::FocusGained => true,
            TuiEvent::Mouse(mouse) => {
                mouse.kind == MouseEventKind::Moved
                    || (pending.shortcut.is_none()
                        && (mouse.kind == MouseEventKind::Up(button)
                            || (mouse.kind == MouseEventKind::Down(button)
                                && mouse.modifiers.is_empty())))
            }
            TuiEvent::Key(key) => {
                pending.shortcut.is_some()
                    && match key.code {
                        KeyCode::Insert => {
                            key.kind == KeyEventKind::Release
                                || key.modifiers == KeyModifiers::SHIFT
                        }
                        KeyCode::Char(c) if c.eq_ignore_ascii_case(&'v') => {
                            key.kind == KeyEventKind::Release
                                || key.modifiers == KeyModifiers::CONTROL
                        }
                        _ => false,
                    }
            }
            TuiEvent::Paste(_) | TuiEvent::FocusLost | TuiEvent::Resume => false,
        };
        if !keep {
            self.pending_mouse_paste = None;
        }
    }

    pub(super) fn finish_mouse_paste(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> TuiEvent {
        if !matches!(event, TuiEvent::Draw) {
            return event;
        }
        let Some(pending) = self.pending_mouse_paste.as_ref() else {
            tui.clipboard.take_text_result();
            return event;
        };
        if self
            .clipboard_paste_target(tui, pending.source, pending.shortcut)
            .as_ref()
            != Some(&pending.target)
        {
            self.pending_mouse_paste = None;
            return event;
        }
        let Some(result) = tui.clipboard.take_text_result() else {
            return event;
        };
        self.pending_mouse_paste = None;
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
