//! Rewind: choose an earlier prompt from the dialogue and revert the conversation to before it.
//!
//! The `rewind` shortcut (`Alt+Z`) leaves the detailed transcript, shows the dialogue lens and
//! highlights the latest prompt, and does nothing while a rewind runs; the lens stays once the
//! rewind ends. The bottom pane's rewind
//! view stands in for the composer and turns arrows, Enter and Escape into [`RewindInput`]s.
//! The selection is the prompt cell's identity, so pagination, prompt reordering and inserted
//! warnings never move it: each owned draw re-derives the highlighted index from it, and ends the
//! rewind once its view, thread or prompt is gone. Confirming hands the prompt to the revert
//! that upstream's Esc-Esc backtracking used; this fork retires that entry.

use std::sync::Arc;

use codex_protocol::ThreadId;
use codex_protocol::models::local_image_label_text;

use super::App;
use super::AppServerSession;
use super::tui;
use crate::app_backtrack::SIDE_EDIT_PREVIOUS_UNAVAILABLE_MESSAGE;
use crate::app_backtrack::nth_user_position;
use crate::app_backtrack::user_count;
use crate::app_event::AppEvent;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::RewindInput;
use crate::chatwidget::UserMessage;
use crate::history_cell::HistoryCell;
use crate::history_cell::UserHistoryCell;

const NO_EARLIER_PROMPT: &str = "No earlier prompt to rewind to.";

pub(super) struct Rewind {
    thread_id: ThreadId,
    /// The highlighted prompt; `None` while older history is searched for one.
    prompt: Option<Arc<dyn HistoryCell>>,
    /// Cancelling returns to the live tail when the rewind began there.
    following: bool,
}

impl App {
    pub(super) fn begin_rewind(&mut self, tui: &mut tui::Tui, app_server: &mut AppServerSession) {
        if self.rewind.is_some() {
            return;
        }
        let thread_id = match self.rewindable_thread(tui) {
            Ok(thread_id) => thread_id,
            Err(refusal) => {
                self.chat_widget
                    .add_info_message(refusal.to_string(), /*hint*/ None);
                return;
            }
        };
        let detailed = self.transcript_view.is_detailed();
        self.transcript_view.set_presentation(
            /*detailed*/ false,
            self.chat_widget.history_render_mode(),
        );
        self.transcript_view.show_dialogue(&self.transcript_cells);
        self.rewind = Some(Rewind {
            thread_id,
            prompt: None,
            following: self.transcript_view.is_following(),
        });
        self.chat_widget.show_rewind();
        if let Some(index) = latest_prompt(&self.transcript_cells) {
            self.select_rewind_prompt(index);
            if detailed {
                // The rows last drawn were detailed, so they cannot tell whether it shows.
                self.transcript_view
                    .jump_to_entry(&self.transcript_cells, index);
            }
        }
        self.request_owned_history(tui, app_server);
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn handle_rewind_input(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        input: RewindInput,
    ) {
        match input {
            RewindInput::Older => self.step_rewind(|nth| nth.checked_sub(1)),
            RewindInput::Newer => self.step_rewind(|nth| nth.checked_add(1)),
            RewindInput::Confirm => self.confirm_rewind(),
            RewindInput::Cancel => self.cancel_rewind(),
        }
        self.request_owned_history(tui, app_server);
        tui.frame_requester().schedule_frame();
    }

    /// Before each owned draw: re-derive the highlight from the selected cell, select the latest
    /// prompt once history yields one, and end the rewind once its view, thread or prompt is gone.
    pub(super) fn sync_rewind(&mut self) {
        let Some(Rewind {
            thread_id, prompt, ..
        }) = &self.rewind
        else {
            return;
        };
        let (thread_id, searching) = (*thread_id, prompt.is_none());
        if !self.chat_widget.shows_rewind() || self.chat_widget.thread_id() != Some(thread_id) {
            self.cancel_rewind();
        } else if !searching {
            match self.rewind_index() {
                Some(index) => self.transcript_view.set_highlight(Some(index)),
                None => self.cancel_rewind(),
            }
        } else if let Some(index) = latest_prompt(&self.transcript_cells) {
            self.select_rewind_prompt(index);
        } else if !self.scrollback_has_older_history {
            self.cancel_rewind();
            self.chat_widget
                .add_info_message(NO_EARLIER_PROMPT.to_string(), /*hint*/ None);
        }
    }

    /// Whether Up would pass the oldest loaded prompt, or none is loaded yet.
    pub(super) fn rewind_needs_history(&self) -> bool {
        let Some(rewind) = &self.rewind else {
            return false;
        };
        rewind.prompt.is_none()
            || self
                .rewind_index()
                .is_some_and(|index| user_count(&self.transcript_cells[..index]) == 0)
    }

    /// The displayed thread when it can rewind now, else why not.
    fn rewindable_thread(&self, tui: &tui::Tui) -> Result<ThreadId, &'static str> {
        if !tui.is_owned_screen() {
            return Err("Rewind needs the fullscreen transcript.");
        }
        if self.chat_widget.side_conversation_active() {
            return Err(SIDE_EDIT_PREVIOUS_UNAVAILABLE_MESSAGE);
        }
        if self.chat_widget.is_external_writer_view() || self.chat_widget.is_parent_owned_thread() {
            return Err("This conversation can't be rewound from here.");
        }
        if self.chat_widget.is_task_running() {
            return Err("Rewind waits for the running turn to finish.");
        }
        self.chat_widget
            .thread_id()
            .filter(|_| user_count(&self.transcript_cells) > 0 || self.scrollback_has_older_history)
            .ok_or(NO_EARLIER_PROMPT)
    }

    /// The selected prompt's index while it is still in the transcript.
    fn rewind_index(&self) -> Option<usize> {
        let prompt = self.rewind.as_ref()?.prompt.as_ref()?;
        self.transcript_cells
            .iter()
            .position(|cell| Arc::ptr_eq(cell, prompt))
    }

    fn select_rewind_prompt(&mut self, index: usize) {
        if let Some(rewind) = &mut self.rewind {
            rewind.prompt = Some(Arc::clone(&self.transcript_cells[index]));
        }
        self.transcript_view.set_highlight(Some(index));
        self.transcript_view
            .ensure_entry_visible(&self.transcript_cells, index);
    }

    fn step_rewind(&mut self, step: impl FnOnce(usize) -> Option<usize>) {
        let Some(index) = self.rewind_index() else {
            return;
        };
        if let Some(target) = step(user_count(&self.transcript_cells[..index]))
            .and_then(|nth| nth_user_position(&self.transcript_cells, nth))
        {
            self.select_rewind_prompt(target);
        }
    }

    fn confirm_rewind(&mut self) {
        // Enter waits while older history is still searched for a prompt.
        if self.rewind_index().is_none() {
            return;
        }
        let Some(Rewind {
            thread_id,
            prompt: Some(selected_cell),
            ..
        }) = self.end_rewind()
        else {
            return;
        };
        self.transcript_view.jump_to_latest();
        let Some(prompt) = selected_cell
            .as_any()
            .downcast_ref::<UserHistoryCell>()
            .map(user_message)
        else {
            return;
        };
        self.app_event_tx
            .send(AppEvent::RevertSessionForPromptEdit {
                thread_id,
                selected_cell,
                prompt,
            });
    }

    fn cancel_rewind(&mut self) {
        if self.end_rewind().is_some_and(|rewind| rewind.following) {
            self.transcript_view.jump_to_latest();
        }
    }

    /// Dismiss the view and clear the highlight, yielding the rewind that ended.
    fn end_rewind(&mut self) -> Option<Rewind> {
        let rewind = self.rewind.take()?;
        self.chat_widget.dismiss_rewind();
        self.transcript_view.set_highlight(/*index*/ None);
        Some(rewind)
    }
}

fn latest_prompt(cells: &[Arc<dyn HistoryCell>]) -> Option<usize> {
    user_count(cells)
        .checked_sub(1)
        .and_then(|nth| nth_user_position(cells, nth))
}

/// The composer draft that resubmits a prompt.
fn user_message(cell: &UserHistoryCell) -> UserMessage {
    UserMessage {
        text: cell.message.clone(),
        local_images: cell
            .local_image_paths
            .iter()
            .enumerate()
            .map(|(index, path)| LocalImageAttachment {
                placeholder: local_image_label_text(index + 1),
                path: path.clone(),
            })
            .collect(),
        remote_image_urls: cell.remote_image_urls.clone(),
        text_elements: cell.text_elements.clone(),
        mention_bindings: Vec::new(),
    }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
