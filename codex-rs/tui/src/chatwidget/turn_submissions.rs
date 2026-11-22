//! Immutable submission previews and lossless draft recovery. RPC ordering belongs to App.

use super::AppCommand;
use super::AppEvent;
use super::ChatWidget;
use super::InputQueueState;
use super::KeyBindingListExt;
use super::KeyCode;
use super::KeyEvent;
use super::KeyEventKind;
use super::KeyModifiers;
use super::LocalImageAttachment;
use super::MessageDelivery;
use super::PendingSteer;
use super::QueuedUserMessage;
use super::ThreadComposerState;
use super::ThreadInputState;
use super::UserInput;
use super::UserMessage;
use super::UserMessageSource;
use super::input_queue;
use super::mention_bindings_from_user_inputs;
use super::merge_user_messages;
use super::user_message_for_restore;
use crate::app::turn_submissions::DraftDisposition;
use crate::app::turn_submissions::DraftOrigin;
use crate::app::turn_submissions::SubmissionDraft;
use crate::app::turn_submissions::SubmissionEvent;
use crate::app::turn_submissions::SubmissionPreview;
use crate::app::turn_submissions::SubmissionPreviewEntry;
use crate::app::turn_submissions::ThreadSubmissionPreview;
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
        matches!(&self.submission_preview.thread, ThreadSubmissionPreview::Sending { waiting, .. } | ThreadSubmissionPreview::Uncertain { waiting, .. } if waiting.iter().any(|entry| matches!(entry, SubmissionPreviewEntry::Message { .. })))
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
            .pending_steer
            .replacement()
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
        let mut previews = match &self.submission_preview.thread {
            ThreadSubmissionPreview::Idle => Vec::new(),
            ThreadSubmissionPreview::Uncertain { active, waiting } => {
                std::iter::once(active.text().to_string())
                    .chain(
                        waiting
                            .iter()
                            .map(|entry| format!("Pending: {}", entry.text())),
                    )
                    .collect()
            }
            ThreadSubmissionPreview::Sending { active, waiting } => {
                std::iter::once(format!("Sending… {}", active.text()))
                    .chain(
                        waiting
                            .iter()
                            .map(|entry| format!("Waiting to send: {}", entry.text())),
                    )
                    .collect()
            }
        };
        if let Some(navigation) = &self.submission_preview.navigation {
            previews.push(match navigation {
                crate::app::turn_submissions::NavigationPreview::Preparing { .. } => {
                    "Preparing navigation…".into()
                }
                crate::app::turn_submissions::NavigationPreview::Cleaning => "Cleaning canceled navigation…".into(),
                crate::app::turn_submissions::NavigationPreview::Uncertain => "Navigation outcome unknown. Close and reopen to resynchronize; the earlier request may still complete.".into(),
            });
        }
        previews
    }

    pub(super) fn edit_waiting_submission(&mut self, action: PendingSteerCancelAction) -> bool {
        if let ThreadSubmissionPreview::Sending { waiting, .. }
        | ThreadSubmissionPreview::Uncertain { waiting, .. } = &self.submission_preview.thread
            && let Some(id) = waiting.iter().rev().find_map(|entry| match entry {
                SubmissionPreviewEntry::Message { id, .. } => Some(*id),
                SubmissionPreviewEntry::Operation { id, .. }
                    if action == PendingSteerCancelAction::Remove =>
                {
                    Some(*id)
                }
                SubmissionPreviewEntry::Operation { .. }
                | SubmissionPreviewEntry::Cancellation { .. } => None,
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
        if action == PendingSteerCancelAction::Remove
            && let Some(crate::app::turn_submissions::NavigationPreview::Preparing { id }) =
                &self.submission_preview.navigation
        {
            self.app_event_tx
                .send(AppEvent::Submission(SubmissionEvent::CancelNavigation {
                    id: *id,
                }));
            return true;
        }
        false
    }

    pub(super) fn explain_sending_submission(&mut self) -> bool {
        if matches!(
            self.submission_preview.thread,
            ThreadSubmissionPreview::Idle
        ) && self.submission_preview.navigation.is_none()
        {
            return false;
        }
        if matches!(
            self.submission_preview.thread,
            ThreadSubmissionPreview::Uncertain { .. }
        ) {
            self.add_info_message("Operation outcome is unknown. Close and reopen this TUI to resynchronize status; the earlier request may still complete. No retry will be sent automatically.".into(), None);
            return true;
        }
        let message = if matches!(
            self.submission_preview.thread,
            ThreadSubmissionPreview::Idle
        ) {
            "Session navigation is preparing or closing; it cannot be edited."
        } else {
            "Message is sending; it cannot be edited until the server answers."
        };
        self.add_info_message(message.into(), /*hint*/ None);
        true
    }

    pub(crate) fn submission_draft_from_op(op: &AppCommand) -> SubmissionDraft {
        let AppCommand::UserTurn {
            client_user_message_id,
            items,
            ..
        } = op
        else {
            unreachable!("user-turn draft")
        };
        Self::submission_draft_from_inputs(
            client_user_message_id.clone(),
            items,
            UserMessageSource::Prompt,
        )
    }

    pub(crate) fn submission_draft_from_inputs(
        client_id: String,
        input: &[UserInput],
        source: UserMessageSource,
    ) -> SubmissionDraft {
        let mut display = Self::user_message_display_from_inputs(input);
        // The readable projection cannot reconstruct question identity after rejected delivery.
        if source == UserMessageSource::QuestionAnswer {
            display.message = input
                .iter()
                .filter_map(|item| {
                    if let UserInput::Text { text, .. } = item {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            display.text_elements.clear();
        }
        let mention_bindings = mention_bindings_from_user_inputs(input, &display.message);
        SubmissionDraft {
            client_id,
            origin: DraftOrigin::Message { source },
            message: UserMessage {
                text: display.message,
                text_elements: display.text_elements,
                local_images: display
                    .local_images
                    .into_iter()
                    .enumerate()
                    .map(|(index, path)| LocalImageAttachment {
                        path,
                        placeholder: codex_protocol::models::local_image_label_text(
                            display.remote_image_urls.len() + index + 1,
                        ),
                    })
                    .collect(),
                remote_image_urls: display.remote_image_urls,
                mention_bindings,
            },
        }
    }

    pub(crate) fn submission_draft(&self, op: &AppCommand) -> SubmissionDraft {
        self.input_queue.submission_draft(op)
    }

    pub(crate) fn cancellation_draft(&self, client_id: &str) -> Option<SubmissionDraft> {
        self.input_queue.cancellation_draft(client_id)
    }

    pub(crate) fn recover_submission_drafts(
        &mut self,
        drafts: Vec<SubmissionDraft>,
        action: DraftDisposition,
    ) {
        let messages = self.input_queue.recover_submission_drafts(drafts, action);
        self.restore_submission_messages(messages);
        self.update_task_running_state();
        self.refresh_pending_input_preview();
    }

    fn restore_submission_messages(&mut self, messages: Vec<UserMessage>) {
        if messages.is_empty() {
            return;
        }
        let draft = self.bottom_pane.composer_draft_snapshot();
        let composer = ThreadComposerState {
            text: draft.text,
            shell_mode: Some(draft.shell_mode.unwrap_or(false)),
            text_elements: draft.text_elements,
            local_images: draft.local_images,
            remote_image_urls: draft.remote_image_urls,
            mention_bindings: draft.mention_bindings,
            pending_pastes: draft.pending_pastes,
        };
        self.restore_composer_state(composer.prepend_messages(messages));
    }

    pub(crate) fn finish_submission_cancel(
        &mut self,
        client_id: &str,
        action: PendingSteerCancelAction,
        cancelled: bool,
    ) {
        match self
            .input_queue
            .finish_submission_cancel(client_id, action, cancelled)
        {
            CancellationEffect::Edit(message) => self.restore_submission_messages(vec![message]),
            CancellationEffect::AlreadySent => {
                self.add_info_message("Pending steer has already been sent.".into(), None);
            }
            CancellationEffect::Unchanged | CancellationEffect::Removed => {}
        }
        self.refresh_pending_input_preview();
        self.request_redraw();
    }
}

impl ThreadInputState {
    pub(crate) fn finish_submission_replacement(
        &mut self,
        draft: &SubmissionDraft,
        replaced: bool,
    ) {
        self.input_queue
            .finish_submission_replacement(draft, replaced);
    }
    pub(crate) fn submission_draft(&self, op: &AppCommand) -> SubmissionDraft {
        self.input_queue.submission_draft(op)
    }
    pub(crate) fn cancellation_draft(&self, client_id: &str) -> Option<SubmissionDraft> {
        self.input_queue.cancellation_draft(client_id)
    }
    pub(crate) fn recover_submission_drafts(
        &mut self,
        drafts: Vec<SubmissionDraft>,
        action: DraftDisposition,
    ) {
        let messages = self.input_queue.recover_submission_drafts(drafts, action);
        if !messages.is_empty() {
            self.composer = Some(
                self.composer
                    .take()
                    .unwrap_or_default()
                    .prepend_messages(messages),
            );
        }
    }
    pub(crate) fn finish_submission_cancel(
        &mut self,
        client_id: &str,
        action: PendingSteerCancelAction,
        cancelled: bool,
    ) {
        if let CancellationEffect::Edit(message) = self
            .input_queue
            .finish_submission_cancel(client_id, action, cancelled)
        {
            self.composer = Some(
                self.composer
                    .take()
                    .unwrap_or_default()
                    .prepend_messages(vec![message]),
            );
        }
    }
}

enum CancellationEffect {
    Unchanged,
    Removed,
    Edit(UserMessage),
    AlreadySent,
}

impl InputQueueState {
    pub(super) fn finish_submission_replacement(
        &mut self,
        draft: &SubmissionDraft,
        replaced: bool,
    ) {
        if self
            .pending_steer
            .replacement()
            .is_some_and(|replacement| replacement.appended.client_id == draft.client_id)
        {
            self.pending_steer.finish_replacement(replaced);
        } else if !replaced
            && let DraftOrigin::Append { previous, appended } = &draft.origin
            && self.pending_steer.client_id() == Some(previous.client_id.as_str())
        {
            self.pending_steer.set(Some((**appended).clone()));
        }
    }

    pub(super) fn submission_draft(&self, op: &AppCommand) -> SubmissionDraft {
        let mut draft = ChatWidget::submission_draft_from_op(op);
        if let Some(replacement) = self.pending_steer.replacement()
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
        } else if let Some(steer) = self.pending_steer.as_ref()
            && draft.client_id == steer.client_id
        {
            draft = steer_draft(&steer);
        } else if let Some(input) = self.turn.submitted()
            && input.client_id == draft.client_id
        {
            draft.message = user_message_for_restore(input.message.clone(), &input.history);
            draft.origin = DraftOrigin::Message {
                source: input.source,
            };
        }
        draft
    }

    pub(super) fn cancellation_draft(&self, client_id: &str) -> Option<SubmissionDraft> {
        self.pending_steer
            .as_ref()
            .filter(|steer| steer.client_id == client_id)
            .map(|steer| steer_draft(&steer))
    }

    pub(super) fn recover_submission_drafts(
        &mut self,
        drafts: Vec<SubmissionDraft>,
        action: DraftDisposition,
    ) -> Vec<UserMessage> {
        let ids: Vec<_> = drafts.iter().map(|draft| &draft.client_id).collect();
        if self
            .turn
            .submitted()
            .is_some_and(|input| ids.contains(&&input.client_id))
        {
            self.turn = input_queue::UserTurnInput::Idle;
        }
        let previous = self
            .pending_steer
            .replacement()
            .filter(|replacement| ids.contains(&&replacement.appended.client_id))
            .map(|replacement| replacement.previous.clone());
        if let Some(previous) = previous {
            self.pending_steer.set(Some(previous));
        }
        recover_pending_steer(&mut self.pending_steer, &drafts);
        match action {
            DraftDisposition::Recover { uncertain_id } => {
                recover_into_queue(
                    &mut self.queued_user_messages,
                    drafts,
                    uncertain_id.as_deref(),
                );
                self.recovered_queue = true;
                Vec::new()
            }
            DraftDisposition::Edit => drafts.into_iter().map(|draft| draft.message).collect(),
            DraftDisposition::Remove => Vec::new(),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "The pending steer identity was checked immediately before taking it."
    )]
    fn finish_submission_cancel(
        &mut self,
        client_id: &str,
        action: PendingSteerCancelAction,
        cancelled: bool,
    ) -> CancellationEffect {
        if self.pending_steer.client_id() != Some(client_id) {
            return CancellationEffect::Unchanged;
        }
        let steer = self.pending_steer.take().expect("matching pending steer");
        if !cancelled {
            return CancellationEffect::AlreadySent;
        }
        if action == PendingSteerCancelAction::Edit {
            CancellationEffect::Edit(user_message_for_restore(
                steer.user_message,
                &steer.history_record,
            ))
        } else {
            CancellationEffect::Removed
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

fn recover_pending_steer(pending: &mut input_queue::SteerInput, drafts: &[SubmissionDraft]) {
    let ids: Vec<_> = drafts.iter().map(|draft| &draft.client_id).collect();
    if pending
        .client_id()
        .is_some_and(|id| ids.iter().any(|candidate| candidate.as_str() == id))
    {
        pending.set(None);
    }
    for draft in drafts {
        if let DraftOrigin::Append { previous, .. } = &draft.origin
            && !ids.contains(&&previous.client_id)
            && pending
                .client_id()
                .is_none_or(|id| id == previous.client_id)
        {
            pending.set(Some((**previous).clone()));
        }
    }
}

fn recover_into_queue(
    queue: &mut VecDeque<QueuedUserMessage>,
    drafts: Vec<SubmissionDraft>,
    uncertain_id: Option<&str>,
) {
    for draft in drafts.into_iter().rev() {
        let delivery = if uncertain_id == Some(draft.client_id.as_str()) {
            if queue.iter().any(|entry| matches!(&entry.delivery, MessageDelivery::Unconfirmed(Some(id)) if id == &draft.client_id)) { continue; }
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

impl ChatWidget {
    pub(crate) fn cancel_unsent_turn_operation(&mut self) {
        if self.input_queue.cancel_unsent_operation() {
            self.update_task_running_state();
            self.request_redraw();
        }
    }
}
