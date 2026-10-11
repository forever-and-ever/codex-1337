//! Incoming follow-ups share pending-input presentation and edit/cancel keys.

use super::*;
use crate::app::QueueEvent;
use crate::app_command::PendingSteerCancelAction;
use codex_app_server_protocol::QueuedSubmission;
use codex_protocol::models::FunctionCallOutputBody;

pub(crate) fn queued_message_text(item: &QueuedSubmission) -> Option<String> {
    if let Some(output) = &item.tool_output {
        let FunctionCallOutputBody::Text(text) = &output.output else {
            return None;
        };
        return Some(
            crate::dynamic_tools::DelegatedMessage::from_tool_output(
                &output.name,
                output.namespace.as_deref(),
                &output.output,
            )
            .map_or_else(|| text.clone(), |message| message.input),
        );
    }
    item.input
        .iter()
        .map(|input| match input {
            codex_app_server_protocol::UserInput::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("\n"))
}

#[cfg(test)]
#[path = "server_queue_tests.rs"]
mod tests;

impl ChatWidget {
    pub(crate) fn update_server_queue(&mut self, items: Vec<QueuedSubmission>, busy: bool) {
        self.input_queue.server_messages = items;
        self.input_queue.server_queue_busy = busy;
        self.refresh_pending_input_preview();
    }

    pub(crate) fn finish_server_queue_removal(&mut self, id: &str, result: &Result<bool, String>) {
        self.input_queue.server_queue_busy = false;
        if result.is_ok() {
            self.input_queue
                .server_messages
                .retain(|item| item.id != id);
        }
        if result == &Ok(false) {
            self.add_info_message(
                "Queued message has already been sent or removed.".into(),
                /*hint*/ None,
            );
        }
        self.refresh_pending_input_preview();
    }

    pub(super) fn server_queue_previews(&self) -> impl Iterator<Item = String> + '_ {
        self.input_queue.server_messages.iter().map(|item| {
            let text = queued_message_text(item).unwrap_or_else(|| "[queued attachment]".into());
            let sender = item
                .tool_output
                .as_ref()
                .and_then(|output| {
                    crate::dynamic_tools::DelegatedMessage::from_tool_output(
                        &output.name,
                        output.namespace.as_deref(),
                        &output.output,
                    )
                })
                .map(|message| {
                    message
                        .source_thread_name
                        .unwrap_or(message.source_thread_id)
                });
            let text =
                sender.map_or_else(|| text.clone(), |sender| format!("From {sender}: {text}"));
            let text = history_cell::sanitize_user_text(text.into()).into_owned();
            if self.input_queue.server_queue_busy {
                format!("Updating… {text}")
            } else {
                text
            }
        })
    }

    pub(super) fn handle_queued_message_key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press || !self.bottom_pane.no_modal_or_popup_active() {
            return false;
        }
        let action = if self.chat_keymap.edit_queued_message.is_pressed(key)
            || self.chat_keymap.edit_pending_steer.is_pressed(key)
                && self.input_queue.pending_steer.is_none()
        {
            PendingSteerCancelAction::Edit
        } else if self.chat_keymap.remove_pending_steer.is_pressed(key)
            && self.input_queue.pending_steer.is_none()
        {
            PendingSteerCancelAction::Remove
        } else {
            return false;
        };
        // Match the displayed order: incoming messages precede local drafts.
        if let Some(composer) = self.pop_latest_queued_composer_state() {
            if action == PendingSteerCancelAction::Edit {
                self.restore_composer_state(composer);
            }
            self.refresh_startup_recovery();
            self.refresh_pending_input_preview();
            return true;
        }
        if self.edit_waiting_submission(action) {
            return true;
        }
        let Some(item) = self.input_queue.server_messages.last().cloned() else {
            if self.explain_sending_submission() {
                return true;
            }
            // Image preparation retains its attachments in the draft when cancelled.
            return self.cancel_image_submission();
        };
        if self.input_queue.server_queue_busy {
            return true;
        }
        if action == PendingSteerCancelAction::Edit && queued_message_text(&item).is_none() {
            self.add_warning_message(
                "This queued attachment cannot be edited here; it can still be cancelled.".into(),
            );
            return true;
        }
        if let Some(thread_id) = self.thread_id() {
            self.input_queue.server_queue_busy = true;
            self.refresh_pending_input_preview();
            self.app_event_tx.send(AppEvent::Queue(QueueEvent::Remove {
                thread_id,
                item,
                action,
            }));
        }
        true
    }
}
