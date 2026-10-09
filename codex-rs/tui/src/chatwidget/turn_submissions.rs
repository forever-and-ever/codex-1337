//! Immutable submission previews and lossless draft recovery. RPC ordering belongs to App.

use super::*;
use crate::app::turn_submissions::DraftDisposition;
use crate::app::turn_submissions::DraftOrigin;
use crate::app::turn_submissions::SubmissionDraft;
use crate::app::turn_submissions::SubmissionEvent;
use crate::app::turn_submissions::SubmissionPreview;
use crate::app::turn_submissions::SubmissionPreviewEntry;
use crate::app_command::PendingSteerCancelAction;
use std::collections::VecDeque;

impl ChatWidget {
    pub(super) fn discard_recovered_submission(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press || !self.chat_keymap.remove_pending_steer.is_pressed(key)
        {
            return false;
        }
        // A recovered draft is local. Discarding it cannot retract a server turn.
        if self.pop_latest_queued_composer_state().is_none() {
            if self.input_queue.pending_steer.take().is_none() {
                return false;
            }
            self.input_queue.recovered_queue &= self.input_queue.has_queued_follow_up_messages();
        }
        self.bottom_pane.handle_restricted_key(
            KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
            crate::bottom_pane::RestrictedInputMode::Disconnected,
        );
        self.refresh_startup_recovery();
        self.refresh_pending_input_preview();
        self.request_redraw();
        true
    }

    pub(crate) fn record_submission_receipt(
        &mut self,
        turn_id: String,
        replay: &AppCommand,
        buffering: Option<&codex_app_server_protocol::ModelSafetyBufferingUpdatedNotification>,
    ) {
        let waiting = buffering
            .filter(|notification| notification.turn_id == turn_id)
            .cloned();
        self.record_safety_buffering_turn(turn_id, replay);
        // A buffering notification can precede the asynchronous start receipt.
        // Replay only its matching turn once the exact retry payload is available.
        if let Some(notification) = waiting {
            self.on_model_safety_buffering_updated(notification, None);
        }
    }
    pub(super) fn has_waiting_submission(&self) -> bool {
        matches!(&self.submission_preview, SubmissionPreview::Sending { waiting, .. } if waiting.iter().any(|entry| matches!(entry, SubmissionPreviewEntry::Message { .. })))
    }

    pub(super) fn edit_submission_after_questions(
        &mut self,
        expanded: bool,
        forward: bool,
    ) -> bool {
        if !expanded || !forward || self.blocks_direct_input {
            return false;
        }
        if self.edit_waiting_submission(PendingSteerCancelAction::Edit) {
            if let Some(questions) = &mut self.bottom_pane.questions {
                questions.set_expanded(false);
            }
            self.request_redraw();
            true
        } else {
            self.explain_sending_submission()
        }
    }
    pub(crate) fn finish_submission_replacement(&mut self, client_id: &str, replaced: bool) {
        if self
            .input_queue
            .pending_steer_replacement
            .as_ref()
            .is_some_and(|pending| pending.appended.client_id == client_id)
        {
            self.finish_pending_steer_replacement(replaced);
        }
    }
    pub(crate) fn update_submission_preview(&mut self, preview: SubmissionPreview) {
        if self.submission_preview == preview {
            return;
        }
        self.submission_preview = preview;
        self.refresh_pending_input_preview();
    }

    pub(super) fn submission_previews(&self) -> Vec<String> {
        match &self.submission_preview {
            SubmissionPreview::Idle => Vec::new(),
            SubmissionPreview::Sending { active, waiting } => {
                std::iter::once(format!("Sending… {}", active.text()))
                    .chain(
                        waiting
                            .iter()
                            .map(|entry| format!("Waiting to send: {}", entry.text())),
                    )
                    .collect()
            }
        }
    }

    pub(super) fn edit_waiting_submission(&mut self, action: PendingSteerCancelAction) -> bool {
        if let SubmissionPreview::Sending { waiting, .. } = &self.submission_preview
            && let Some(id) = waiting.iter().rev().find_map(|entry| match entry {
                SubmissionPreviewEntry::Message { id, .. } => Some(*id),
                SubmissionPreviewEntry::Cancellation { .. } => None,
            })
            && let Some(thread_id) = self.thread_id()
        {
            self.app_event_tx
                .send(AppEvent::Submission(SubmissionEvent::Edit {
                    thread_id,
                    id,
                    action,
                }));
            return true;
        }
        false
    }

    pub(super) fn explain_sending_submission(&mut self) -> bool {
        if matches!(self.submission_preview, SubmissionPreview::Idle) {
            return false;
        }
        self.add_info_message(
            "Message is sending; it cannot be edited until the server answers.".into(),
            /*hint*/ None,
        );
        true
    }

    pub(crate) fn submission_draft_from_op(op: &AppCommand) -> SubmissionDraft {
        let AppCommand::UserTurn {
            client_user_message_id,
            items,
            pending_steer_replacement,
            ..
        } = op
        else {
            unreachable!("user-turn draft")
        };
        let _ = pending_steer_replacement;
        Self::submission_draft_from_inputs(client_user_message_id.clone(), items)
    }

    pub(crate) fn submission_draft_from_inputs(
        client_id: String,
        input: &[codex_app_server_protocol::UserInput],
    ) -> SubmissionDraft {
        let display = Self::user_message_display_from_inputs(input);
        let mention_bindings = mention_bindings_from_user_inputs(input, &display.message);
        SubmissionDraft {
            client_id,
            origin: DraftOrigin::Message {
                source: UserMessageSource::Prompt,
            },
            message: UserMessage {
                text: display.message,
                text_elements: display.text_elements,
                local_images: display
                    .local_images
                    .into_iter()
                    .enumerate()
                    .map(|(index, path)| LocalImageAttachment {
                        path,
                        placeholder: codex_protocol::models::local_image_label_text(index + 1),
                    })
                    .collect(),
                remote_image_urls: display.remote_image_urls,
                mention_bindings,
            },
        }
    }

    pub(crate) fn submission_draft(&self, op: &AppCommand) -> SubmissionDraft {
        let mut draft = Self::submission_draft_from_op(op);
        if let Some(replacement) = &self.input_queue.pending_steer_replacement
            && replacement.appended.client_id == draft.client_id
        {
            draft.message = user_message_for_restore(
                replacement.appended.user_message.clone(),
                &replacement.appended.history_record,
            );
            draft.origin = DraftOrigin::Append {
                previous: Box::new(replacement.previous.clone()),
                appended: Box::new(replacement.appended.clone()),
            };
        } else if let Some(steer) = &self.input_queue.pending_steer
            && draft.client_id == steer.client_id
        {
            draft.message =
                user_message_for_restore(steer.user_message.clone(), &steer.history_record);
            draft.origin = DraftOrigin::Message {
                source: steer.source,
            };
        } else if self.input_queue.pending_user_message_client_id.as_ref() == Some(&draft.client_id)
            && let Some(message) = &self.safety_buffering_prompt
        {
            draft.message = message.clone();
            draft.origin = DraftOrigin::Message {
                source: self.safety_buffering_source,
            };
        }
        draft
    }

    pub(crate) fn cancellation_draft(&self, client_id: &str) -> Option<SubmissionDraft> {
        self.input_queue
            .pending_steer
            .as_ref()
            .filter(|steer| steer.client_id == client_id)
            .map(steer_draft)
    }

    pub(crate) fn recover_submission_drafts(
        &mut self,
        drafts: Vec<SubmissionDraft>,
        action: DraftDisposition,
    ) {
        let ids: Vec<_> = drafts.iter().map(|draft| &draft.client_id).collect();
        if self
            .input_queue
            .pending_user_message_client_id
            .as_ref()
            .is_some_and(|id| ids.contains(&id))
        {
            self.input_queue.user_turn_pending_start = false;
            self.input_queue.pending_user_message_client_id = None;
            self.safety_buffering_prompt = None;
        }
        if self
            .input_queue
            .pending_steer_replacement
            .as_ref()
            .is_some_and(|replacement| ids.contains(&&replacement.appended.client_id))
        {
            self.input_queue.pending_steer_replacement = None;
        }
        recover_pending_steer(&mut self.input_queue.pending_steer, &drafts);
        match action {
            DraftDisposition::Recover { uncertain_id } => {
                recover_into_queue(
                    &mut self.input_queue.queued_user_messages,
                    &mut self.input_queue.queued_user_message_history_records,
                    drafts,
                    uncertain_id.as_deref(),
                );
                self.input_queue.recovered_queue = true;
            }
            DraftDisposition::Edit => {
                for draft in drafts.into_iter().rev() {
                    self.restore_user_message_to_composer(draft.message);
                }
            }
            DraftDisposition::Remove => {}
        }
        self.update_task_running_state();
        self.refresh_pending_input_preview();
    }

    pub(crate) fn finish_submission_cancel(
        &mut self,
        client_id: &str,
        action: PendingSteerCancelAction,
        cancelled: bool,
    ) {
        if self.pending_steer_client_id() != Some(client_id) {
            return;
        }
        if cancelled {
            self.finish_pending_steer_cancellation(action);
        } else {
            self.discard_stale_pending_steer(client_id);
            self.add_info_message(
                "Pending steer has already been sent.".into(),
                /*hint*/ None,
            );
        }
    }
}

impl ThreadInputState {
    pub(crate) fn finish_submission_replacement(
        &mut self,
        draft: &SubmissionDraft,
        replaced: bool,
    ) {
        if !replaced
            && let DraftOrigin::Append { previous, appended } = &draft.origin
            && self
                .pending_steer
                .as_ref()
                .is_some_and(|pending| pending.client_id == previous.client_id)
        {
            self.pending_steer = Some((**appended).clone());
        }
    }
    pub(crate) fn cancellation_draft(&self, client_id: &str) -> Option<SubmissionDraft> {
        self.pending_steer
            .as_ref()
            .filter(|steer| steer.client_id == client_id)
            .map(steer_draft)
    }

    pub(crate) fn submission_draft(&self, op: &AppCommand) -> SubmissionDraft {
        let mut draft = ChatWidget::submission_draft_from_op(op);
        if let Some(steer) = &self.pending_steer
            && draft.client_id == steer.client_id
        {
            draft.message =
                user_message_for_restore(steer.user_message.clone(), &steer.history_record);
            draft.origin = DraftOrigin::Message {
                source: steer.source,
            };
        } else if self.pending_user_message_client_id.as_ref() == Some(&draft.client_id)
            && let Some(message) = &self.safety_buffering_prompt
        {
            draft.message = message.clone();
            draft.origin = DraftOrigin::Message {
                source: self.safety_buffering_source,
            };
        }
        draft
    }

    pub(crate) fn recover_submission_drafts(
        &mut self,
        drafts: Vec<SubmissionDraft>,
        action: DraftDisposition,
    ) {
        let ids: Vec<_> = drafts.iter().map(|draft| &draft.client_id).collect();
        if self
            .pending_user_message_client_id
            .as_ref()
            .is_some_and(|id| ids.contains(&id))
        {
            self.user_turn_pending_start = false;
            self.pending_user_message_client_id = None;
            self.safety_buffering_prompt = None;
        }
        recover_pending_steer(&mut self.pending_steer, &drafts);
        if let DraftDisposition::Recover { uncertain_id } = action {
            recover_into_queue(
                &mut self.queued_user_messages,
                &mut self.queued_user_message_history_records,
                drafts,
                uncertain_id.as_deref(),
            );
            self.recovered_queue = true;
        } else if action == DraftDisposition::Edit {
            let composer = self.composer.take().unwrap_or_default();
            let messages = drafts
                .into_iter()
                .map(|draft| draft.message)
                .chain(std::iter::once(UserMessage {
                    text: composer.text,
                    text_elements: composer.text_elements,
                    local_images: composer.local_images,
                    remote_image_urls: composer.remote_image_urls,
                    mention_bindings: composer.mention_bindings,
                }))
                .collect();
            self.composer = Some(ChatWidget::composer_state_from_user_message(
                merge_user_messages(messages),
                composer.pending_pastes,
            ));
        }
    }

    pub(crate) fn finish_submission_cancel(
        &mut self,
        client_id: &str,
        action: PendingSteerCancelAction,
        cancelled: bool,
    ) {
        if self
            .pending_steer
            .as_ref()
            .is_none_or(|steer| steer.client_id != client_id)
        {
            return;
        }
        let Some(steer) = self.pending_steer.take() else {
            return;
        };
        if cancelled && action == PendingSteerCancelAction::Edit {
            self.recover_submission_drafts(
                vec![SubmissionDraft {
                    client_id: client_id.to_owned(),
                    origin: DraftOrigin::Message {
                        source: steer.source,
                    },
                    message: user_message_for_restore(steer.user_message, &steer.history_record),
                }],
                action.into(),
            );
        }
    }
}

impl SubmissionDraft {
    pub(crate) fn merge_waiting(self, appended: Self) -> Self {
        Self {
            client_id: self.client_id,
            message: merge_user_messages(vec![self.message, appended.message]),
            origin: self.origin,
        }
    }
}

fn recover_pending_steer(pending: &mut Option<PendingSteer>, drafts: &[SubmissionDraft]) {
    let ids: Vec<_> = drafts.iter().map(|draft| &draft.client_id).collect();
    if pending
        .as_ref()
        .is_some_and(|steer| ids.contains(&&steer.client_id))
    {
        *pending = None;
    }
    for draft in drafts {
        if let DraftOrigin::Append { previous, .. } = &draft.origin
            && !ids.contains(&&previous.client_id)
            && pending
                .as_ref()
                .is_none_or(|steer| steer.client_id == previous.client_id)
        {
            *pending = Some((**previous).clone());
        }
    }
}

fn recover_into_queue(
    queue: &mut VecDeque<QueuedUserMessage>,
    history: &mut VecDeque<UserMessageHistoryRecord>,
    drafts: Vec<SubmissionDraft>,
    uncertain_id: Option<&str>,
) {
    for draft in drafts.into_iter().rev() {
        let delivery = if uncertain_id == Some(draft.client_id.as_str()) {
            if queue.iter().any(|entry| matches!(&entry.delivery, MessageDelivery::Unconfirmed(Some(id)) if id == &draft.client_id)) {
                continue;
            }
            MessageDelivery::Unconfirmed(Some(draft.client_id))
        } else {
            MessageDelivery::Unsent
        };
        let source = match &draft.origin {
            DraftOrigin::Append { appended, .. } => appended.source,
            DraftOrigin::Message { source } => *source,
        };
        queue.push_front(QueuedUserMessage {
            delivery,
            source,
            ..QueuedUserMessage::from(draft.message)
        });
        history.push_front(UserMessageHistoryRecord::UserMessageText);
    }
}
fn steer_draft(steer: &PendingSteer) -> SubmissionDraft {
    SubmissionDraft {
        client_id: steer.client_id.clone(),
        message: user_message_for_restore(steer.user_message.clone(), &steer.history_record),
        origin: DraftOrigin::Message {
            source: steer.source,
        },
    }
}
