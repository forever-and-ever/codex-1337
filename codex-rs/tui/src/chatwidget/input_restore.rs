//! Input queue restore and thread-input snapshot behavior for `ChatWidget`.

use std::collections::HashSet;

use crate::bottom_pane::ComposerDraftSnapshot;
use crate::bottom_pane::KillBufferSnapshot;

use super::user_messages::remap_colliding_paste_placeholders;
use super::*;

impl ThreadInputState {
    pub(crate) fn restore_queued_text(&mut self, text: String) {
        self.composer = Some(
            self.composer
                .take()
                .unwrap_or_default()
                .prepend_messages(vec![UserMessage::from(text)]),
        );
    }
}

impl ChatWidget {
    pub(crate) fn take_kill_buffer_snapshot(&mut self) -> KillBufferSnapshot {
        self.bottom_pane.take_kill_buffer_snapshot()
    }

    pub(crate) fn restore_kill_buffer_snapshot(&mut self, snapshot: KillBufferSnapshot) {
        self.bottom_pane.restore_kill_buffer_snapshot(snapshot);
    }

    /// Restore the exact draft entered before the fully initialized composer became available.
    pub(crate) fn restore_startup_draft(&mut self, draft: ComposerDraftSnapshot) {
        self.bottom_pane
            .inherit_startup_sparkle(draft.sparkle_draft);
        let existing_draft = self.bottom_pane.composer_draft_snapshot();
        let existing_cursor = existing_draft.cursor;
        let existing_message = UserMessage {
            text: existing_draft.text,
            text_elements: existing_draft.text_elements,
            local_images: existing_draft.local_images,
            remote_image_urls: existing_draft.remote_image_urls,
            mention_bindings: existing_draft.mention_bindings,
        };
        let existing_has_content =
            !self.bottom_pane.composer_is_empty() || !existing_draft.pending_pastes.is_empty();

        let startup_message = UserMessage {
            text: draft.text,
            text_elements: draft.text_elements,
            local_images: draft.local_images,
            remote_image_urls: draft.remote_image_urls,
            mention_bindings: draft.mention_bindings,
        };
        let startup_has_content = !startup_message.text.is_empty()
            || !startup_message.local_images.is_empty()
            || !startup_message.remote_image_urls.is_empty()
            || !draft.pending_pastes.is_empty();

        let cursor = if existing_has_content && startup_has_content {
            let mut used_paste_placeholders = HashSet::new();
            let (existing_message, mut pending_pastes) = remap_colliding_paste_placeholders(
                existing_message,
                existing_draft.pending_pastes,
                &mut used_paste_placeholders,
            );
            let startup_offset = existing_message.text.len().saturating_add(1);
            let preceding_text_element = startup_message
                .text_elements
                .iter()
                .enumerate()
                .take_while(|(_, element)| element.byte_range.end <= draft.cursor)
                .last()
                .map(|(index, element)| (index, element.byte_range.end));
            let (startup_message, startup_pending_pastes) = remap_colliding_paste_placeholders(
                startup_message,
                draft.pending_pastes,
                &mut used_paste_placeholders,
            );
            let cursor_adjustment = preceding_text_element.map_or(0, |(index, original_end)| {
                startup_message.text_elements[index]
                    .byte_range
                    .end
                    .saturating_sub(original_end)
            });
            pending_pastes.extend(startup_pending_pastes);
            self.restore_composer_state(Self::composer_state_from_user_message(
                merge_user_messages(vec![existing_message, startup_message]),
                pending_pastes,
            ));
            startup_offset
                .saturating_add(draft.cursor)
                .saturating_add(cursor_adjustment)
        } else if existing_has_content || !startup_has_content {
            existing_cursor
        } else {
            self.restore_composer_state(Self::composer_state_from_user_message(
                startup_message,
                draft.pending_pastes,
            ));
            draft.cursor
        };
        self.bottom_pane.set_composer_cursor(cursor);
        self.bottom_pane.restore_startup_composer_state(
            draft.last_composer_activity_at,
            draft.startup_local_history,
        );
        if startup_has_content && self.local_settings.tui.vim_mode_default {
            self.bottom_pane.enable_vim_in_insert_mode();
        }
        crate::startup_recovery::handed_off(|| self.bottom_pane.composer_recovery_snapshot());
    }

    /// Includes protected prompts deferred by streaming or the approval idle timer.
    pub(crate) fn has_pending_protected_request(&self) -> bool {
        self.bottom_pane.has_pending_approval() || self.interrupts.has_pending_prompt()
    }

    /// Transfer startup input only after protected views and required sandbox setup finish.
    pub(crate) fn restore_startup_draft_when_ready(
        &mut self,
        pending_draft: &mut Option<ComposerDraftSnapshot>,
    ) {
        if let Some(draft) = pending_draft.as_ref() {
            self.bottom_pane
                .inherit_startup_sparkle(draft.sparkle_draft);
        }
        if self.has_active_view()
            || self
                .bottom_pane
                .questions
                .as_ref()
                .is_some_and(|q| q.expanded)
            || self.has_pending_protected_request()
            || !self.bottom_pane.composer_input_enabled()
        {
            return;
        }
        #[cfg(any(target_os = "windows", test))]
        if self.windows_sandbox_host == crate::app::WindowsSandboxHost::Local
            && (self.windows_sandbox_local_server
                && self.windows_sandbox_config.requirements.is_none()
                || self.elevated_windows_sandbox_setup_required())
        {
            return;
        }
        if let Some(draft) = pending_draft.take() {
            self.restore_startup_draft(draft);
        }
    }

    pub(crate) fn set_initial_user_message_submit_suppressed(&mut self, suppressed: bool) {
        self.suppress_initial_user_message_submit = suppressed;
    }

    pub(crate) fn submit_initial_user_message_if_pending(&mut self) {
        if self.suppress_initial_user_message_submit || self.input_queue.rate_limit_recovery_pending
        {
            return;
        }
        #[cfg(any(target_os = "windows", test))]
        if self.windows_sandbox_local_server
            && self.windows_sandbox_host != crate::app::WindowsSandboxHost::Remote
            && self.windows_sandbox_config.requirements.is_none()
        {
            return;
        }
        #[cfg(any(target_os = "windows", test))]
        if self.windows_sandbox_host == crate::app::WindowsSandboxHost::Local
            && self.elevated_windows_sandbox_setup_required()
        {
            return;
        }
        #[cfg(any(target_os = "windows", test))]
        if matches!(
            self.windows_sandbox_host,
            crate::app::WindowsSandboxHost::Mixed | crate::app::WindowsSandboxHost::Unknown
        ) && self.elevated_windows_sandbox_setup_required()
        {
            if let Some(user_message) = self.initial_user_message.take() {
                self.restore_user_message_to_composer(user_message);
            }
            return;
        }
        if self.blocks_direct_input {
            if let Some(user_message) = self.initial_user_message.take() {
                self.restore_user_message_to_composer(user_message);
            }
            return;
        }
        if let Some(user_message) = self.initial_user_message.take() {
            self.submit_user_message(user_message);
        }
    }

    pub(super) fn pop_next_queued_user_message(
        &mut self,
    ) -> Option<(QueuedUserMessage, UserMessageHistoryRecord)> {
        let message = self.input_queue.pop_next()?;
        let history = message.history_record.clone();
        Some((message, history))
    }

    pub(super) fn pop_latest_queued_composer_state(&mut self) -> Option<ThreadComposerState> {
        self.input_queue.pop_latest()
    }

    pub(crate) fn enqueue_rejected_steer(&mut self) -> bool {
        let Some(steer) = self.input_queue.pending_steer.take() else {
            tracing::warn!(
                "received active-turn-not-steerable error without a matching pending steer"
            );
            return false;
        };
        self.input_queue
            .rejected_steers_queue
            .push_back(QueuedUserMessage {
                source: steer.source,
                history_record: steer.history_record,
                ..QueuedUserMessage::from(steer.user_message)
            });
        self.refresh_pending_input_preview();
        true
    }

    pub(crate) fn finish_pending_steer_replacement(&mut self, replaced: bool) {
        self.input_queue.pending_steer.finish_replacement(replaced);
        self.refresh_pending_input_preview();
        self.request_redraw();
    }

    pub(crate) fn abort_pending_steer_replacement(&mut self) -> bool {
        let Some(appended) = self.input_queue.pending_steer.abort_replacement() else {
            return false;
        };
        self.restore_user_message_to_composer(user_message_for_restore(
            appended.user_message,
            &appended.history_record,
        ));
        self.refresh_pending_input_preview();
        self.request_redraw();
        true
    }

    pub(crate) fn finish_pending_steer_cancellation(
        &mut self,
        action: crate::app_command::PendingSteerCancelAction,
    ) -> bool {
        let Some(client_id) = self
            .input_queue
            .pending_steer
            .client_id()
            .map(str::to_owned)
        else {
            return false;
        };
        self.finish_submission_cancel(&client_id, action, true);
        true
    }

    pub(crate) fn discard_stale_pending_steer(&mut self, client_id: &str) -> bool {
        if self.input_queue.pending_steer.client_id() != Some(client_id) {
            return false;
        }
        self.input_queue.pending_steer.set(None);
        self.refresh_pending_input_preview();
        self.request_redraw();
        true
    }

    /// Handle a turn aborted due to user interrupt, budget exhaustion,
    /// or review completion.
    /// When there are queued user messages, restore them into the composer
    /// separated by newlines rather than auto-submitting the next one.
    pub(super) fn on_interrupted_turn(&mut self, reason: TurnAbortReason) {
        self.requeue_image_submission();
        // Finalize, log a gentle prompt, and clear running state.
        self.finalize_turn();
        let send_pending_steer_immediately =
            self.input_queue.pending_steer.interrupt() == input_queue::SteerInterrupt::SubmitAsTurn;
        self.input_queue
            .pending_steer
            .set_interrupt(input_queue::SteerInterrupt::RestoreDraft);
        self.input_queue.pending_steer.finish_replacement(true);
        if self.interrupted_turn_notice_mode != InterruptedTurnNoticeMode::Suppress {
            if send_pending_steer_immediately {
                self.add_to_history(history_cell::new_info_event(
                    "Model interrupted to submit steer instructions.".to_owned(),
                    /*hint*/ None,
                ));
            } else {
                self.add_to_history(match reason {
                    TurnAbortReason::BudgetLimited => history_cell::new_error_event(
                        "Goal budget reached - the turn was stopped.".to_string(),
                    ),
                    TurnAbortReason::Interrupted => PlainHistoryCell::new(vec![
                        Line::from(
                            "■ Conversation interrupted - use /feedback if something went wrong",
                        )
                        .style(crate::style::secondary_text_style()),
                    ]),
                });
            }
        }

        // The server has already discarded pending input by the time the
        // interrupted turn reaches the UI, so any unacknowledged steers still
        // tracked here must be restored locally instead of waiting for a later commit.
        if send_pending_steer_immediately {
            if let Some(pending) = self.input_queue.pending_steer.take() {
                self.submit_user_message_with_history_and_shell_escape_policy(
                    pending.user_message,
                    pending.history_record,
                    ShellEscapePolicy::Allow,
                    pending.source,
                );
            } else if let Some(combined) = self.drain_pending_messages_for_restore() {
                self.restore_composer_state(combined);
            }
        } else if let Some(combined) = self.drain_pending_messages_for_restore() {
            self.restore_composer_state(combined);
        }
        self.refresh_pending_input_preview();
        self.request_redraw();
    }

    pub(super) fn drain_pending_messages_for_restore(&mut self) -> Option<ThreadComposerState> {
        let pending = self.input_queue.drain_for_restore()?;
        let composer = self.bottom_pane.composer_draft_snapshot();
        let current = ThreadComposerState {
            text: composer.text,
            text_elements: composer.text_elements,
            local_images: composer.local_images,
            remote_image_urls: composer.remote_image_urls,
            mention_bindings: composer.mention_bindings,
            pending_pastes: composer.pending_pastes,
        };
        Some(ThreadComposerState::merge(
            pending.into_iter().chain(std::iter::once(current)),
        ))
    }

    pub(crate) fn restore_user_message_to_composer(&mut self, user_message: UserMessage) {
        let draft = self.bottom_pane.composer_draft_snapshot();
        let composer = ThreadComposerState {
            text: draft.text,
            text_elements: draft.text_elements,
            local_images: draft.local_images,
            remote_image_urls: draft.remote_image_urls,
            mention_bindings: draft.mention_bindings,
            pending_pastes: draft.pending_pastes,
        };
        self.restore_composer_state(composer.prepend_messages(vec![user_message]));
    }

    pub(super) fn restore_composer_state(&mut self, composer: ThreadComposerState) {
        let ThreadComposerState {
            text,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
            pending_pastes,
        } = composer;
        let local_image_paths = local_images.into_iter().map(|img| img.path).collect();
        self.set_remote_image_urls(remote_image_urls);
        self.bottom_pane.set_composer_text_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.bottom_pane.set_composer_pending_pastes(pending_pastes);
    }

    pub(super) fn composer_state_from_user_message(
        user_message: UserMessage,
        pending_pastes: Vec<(String, String)>,
    ) -> ThreadComposerState {
        let UserMessage {
            text,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
        } = user_message;
        ThreadComposerState {
            text,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
            pending_pastes,
        }
    }

    pub(crate) fn capture_thread_input_state(&mut self) -> Option<ThreadInputState> {
        self.cancel_image_submission();
        self.bottom_pane.flush_composer_paste_burst();
        let draft = self.bottom_pane.composer_draft_snapshot();
        let composer = ThreadComposerState {
            text: draft.text,
            text_elements: draft.text_elements,
            local_images: draft.local_images,
            remote_image_urls: draft.remote_image_urls,
            mention_bindings: draft.mention_bindings,
            pending_pastes: draft.pending_pastes,
        };
        Some(ThreadInputState {
            questions: self
                .bottom_pane
                .questions
                .as_deref_mut()
                .map(crate::bottom_pane::AsyncQuestions::capture),
            pending_thread_settings: None,
            composer: composer.has_content().then_some(composer),
            input_queue: self.input_queue.clone(),
            reconnect_pending: false,
            current_collaboration_mode: self.current_collaboration_mode.clone(),
            active_collaboration_mask: self.active_collaboration_mask.clone(),
            plan_mode_reasoning_effort: self.config.plan_mode_reasoning_effort.clone(),
            task_running: self.bottom_pane.is_task_running(),
            agent_turn_running: self.turn_lifecycle.agent_turn_running,
        })
    }

    pub(crate) fn restore_thread_input_state(
        &mut self,
        input_state: Option<ThreadInputState>,
        restore_mode: ThreadInputStateRestoreMode,
    ) {
        let preserve_in_flight_turn = restore_mode.preserve_in_flight_turn;
        let restored_task_running =
            preserve_in_flight_turn && input_state.as_ref().is_some_and(|state| state.task_running);
        if let Some(input_state) = input_state {
            self.bottom_pane.restore_questions(input_state.questions);
            self.current_collaboration_mode = input_state.current_collaboration_mode;
            self.active_collaboration_mask = input_state.active_collaboration_mask;
            self.config.plan_mode_reasoning_effort = input_state.plan_mode_reasoning_effort;
            self.turn_lifecycle.restore_running(
                preserve_in_flight_turn && input_state.agent_turn_running,
                Instant::now(),
            );
            self.input_queue = input_state.input_queue;
            if !preserve_in_flight_turn {
                self.input_queue
                    .recover_inactive(input_state.reconnect_pending);
            }
            self.update_collaboration_mode_indicator();
            self.refresh_model_dependent_surfaces();
            self.restore_composer_state(input_state.composer.unwrap_or_default());
        } else {
            self.turn_lifecycle.restore_running(false, Instant::now());
            self.input_queue.clear();
            self.input_queue_frontend.clear();
            self.restore_composer_state(Default::default());
        }
        self.input_queue.recovered_queue &= self.input_queue.has_queued_follow_up_messages()
            || self.input_queue.pending_steer.is_some();
        let effort = self.effective_reasoning_effort();
        self.bottom_pane
            .set_active_reasoning_effort_baseline(effort.as_ref());
        self.turn_lifecycle
            .restore_running(self.turn_lifecycle.agent_turn_running, Instant::now());
        self.update_task_running_state();
        if restored_task_running && !self.bottom_pane.is_task_running() {
            self.bottom_pane.set_task_running(true);
            self.refresh_status_surfaces();
        }
        self.refresh_pending_input_preview();
        self.request_redraw();
    }

    pub(crate) fn set_queue_autosend_suppressed(&mut self, suppressed: bool) {
        #[cfg(target_os = "linux")]
        let suppressed = suppressed || crate::reload::worker::is_activating();
        self.input_queue_frontend.suppress_queue_autosend = suppressed;
    }
}

impl ThreadComposerState {
    pub(super) fn prepend_messages(self, messages: Vec<UserMessage>) -> Self {
        Self::merge(
            messages
                .into_iter()
                .map(|message| ChatWidget::composer_state_from_user_message(message, Vec::new()))
                .chain(self.has_content().then_some(self)),
        )
    }

    pub(super) fn merge(states: impl IntoIterator<Item = Self>) -> Self {
        let mut used = HashSet::new();
        let mut messages = Vec::new();
        let mut pastes = Vec::new();
        for state in states {
            let message = UserMessage {
                text: state.text,
                text_elements: state.text_elements,
                local_images: state.local_images,
                remote_image_urls: state.remote_image_urls,
                mention_bindings: state.mention_bindings,
            };
            let (message, pending_pastes) =
                remap_colliding_paste_placeholders(message, state.pending_pastes, &mut used);
            messages.push(message);
            pastes.extend(pending_pastes);
        }
        ChatWidget::composer_state_from_user_message(merge_user_messages(messages), pastes)
    }
}

impl InputQueueState {
    pub(super) fn pop_next(&mut self) -> Option<QueuedUserMessage> {
        if self.rejected_steers_queue.is_empty() {
            return self.queued_user_messages.pop_front();
        }
        let count = self
            .rejected_steers_queue
            .iter()
            .position(|message| crate::async_question_reply::parse(&message.text).is_some())
            .map(|index| index.max(1))
            .unwrap_or(self.rejected_steers_queue.len());
        let messages: Vec<_> = self.rejected_steers_queue.drain(..count).collect();
        let source = if messages
            .iter()
            .all(|message| message.source == UserMessageSource::QuestionAnswer)
        {
            UserMessageSource::QuestionAnswer
        } else {
            UserMessageSource::Prompt
        };
        let (message, history_record) = merge_user_messages_with_history_record(
            messages
                .into_iter()
                .map(|message| (message.user_message, message.history_record))
                .collect(),
        );
        Some(QueuedUserMessage {
            source,
            history_record,
            ..QueuedUserMessage::from(message)
        })
    }

    pub(super) fn pop_latest(&mut self) -> Option<ThreadComposerState> {
        let message = self
            .queued_user_messages
            .pop_back()
            .or_else(|| self.rejected_steers_queue.pop_back())?;
        self.recovered_queue &=
            self.has_queued_follow_up_messages() || self.pending_steer.is_some();
        Some(ChatWidget::composer_state_from_user_message(
            user_message_for_restore(message.user_message, &message.history_record),
            message.pending_pastes,
        ))
    }

    pub(super) fn drain_for_restore(&mut self) -> Option<Vec<ThreadComposerState>> {
        if self.pending_steer.is_none() && !self.has_queued_follow_up_messages() {
            return None;
        }
        let steer = self.pending_steer.take().map(|steer| QueuedUserMessage {
            source: steer.source,
            history_record: steer.history_record,
            ..QueuedUserMessage::from(steer.user_message)
        });
        Some(
            self.rejected_steers_queue
                .drain(..)
                .chain(steer)
                .chain(self.queued_user_messages.drain(..))
                .map(|message| {
                    ChatWidget::composer_state_from_user_message(
                        user_message_for_restore(message.user_message, &message.history_record),
                        message.pending_pastes,
                    )
                })
                .collect(),
        )
    }

    pub(super) fn recover_inactive(&mut self, uncertain: bool) {
        if let Some(steer) = self.pending_steer.take() {
            self.queued_user_messages.push_front(QueuedUserMessage {
                source: steer.source,
                history_record: steer.history_record,
                delivery: if uncertain {
                    MessageDelivery::Unconfirmed(Some(steer.client_id))
                } else {
                    MessageDelivery::Unsent
                },
                ..QueuedUserMessage::from(steer.user_message)
            });
        }
        self.turn.started();
    }
}
