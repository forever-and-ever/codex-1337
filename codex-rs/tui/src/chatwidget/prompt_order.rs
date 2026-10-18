use super::ChatWidget;
use super::UserMessage;
use super::UserMessageDisplay;
use super::UserMessageHistoryRecord;
use super::user_messages::user_message_display_for_history;
use crate::app_event::AppEvent;
use crate::history_cell;
use crate::prompt_order::UserPromptIdentity;

impl ChatWidget {
    pub(super) fn on_submitted_user_message_display(
        &mut self,
        message: UserMessage,
        history: &UserMessageHistoryRecord,
        client_id: Option<&str>,
    ) {
        let Some(client_id) = client_id else {
            return;
        };
        self.on_identified_user_message_display(
            user_message_display_for_history(message, history),
            UserPromptIdentity {
                client_id: Some(client_id.to_owned()),
                source_response_item_id: None,
            },
            /*timestamp*/ None,
        );
    }

    pub(super) fn on_identified_user_message_display(
        &mut self,
        display: UserMessageDisplay,
        identity: UserPromptIdentity,
        timestamp: Option<history_cell::HistoryTimestamp>,
    ) {
        self.transcript.last_status_copy_targets = None;
        self.last_rendered_user_message_display = Some(display.clone());
        self.last_rendered_user_message_client_id = None;
        if !display.message.trim().is_empty()
            || !display.text_elements.is_empty()
            || !display.local_images.is_empty()
            || !display.remote_image_urls.is_empty()
        {
            let mut cell = history_cell::new_user_prompt(
                display.message,
                display.text_elements,
                display.local_images,
                display.remote_image_urls,
            );
            cell.prompt_identity = identity;
            cell.timestamp = timestamp;
            self.add_to_history(cell);
        }
    }

    pub(super) fn bind_user_prompt(
        &self,
        identity: UserPromptIdentity,
        timestamp: Option<history_cell::HistoryTimestamp>,
    ) {
        if identity.client_id.is_some()
            && (identity.source_response_item_id.is_some() || timestamp.is_some())
            && let Some(thread_id) = self.thread_id
        {
            self.app_event_tx.send(AppEvent::BindUserPrompt {
                thread_id,
                identity,
                timestamp,
            });
        }
    }
}
