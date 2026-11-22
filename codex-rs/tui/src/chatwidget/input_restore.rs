//! Input queue restore and thread-input snapshot behavior for `ChatWidget`.

use std::collections::HashSet;
use std::collections::VecDeque;

use codex_protocol::models::local_image_label_text;

use crate::bottom_pane::ComposerDraftSnapshot;
use crate::bottom_pane::KillBufferSnapshot;
use crate::bottom_pane::MentionUsage;
use crate::bottom_pane::ShellPrefixHandling;

use super::user_messages::remap_colliding_paste_placeholders;
use super::*;

#[derive(Debug, Clone)]
pub(crate) enum ThreadInputSettingsAuthority {
    Retained,
    Session {
        accepted_mode: Option<CollaborationMode>,
    },
}

impl ThreadInputState {
    pub(crate) fn restore_queued_text(&mut self, text: String) {
        self.composer = Some(
            self.composer
                .take()
                .unwrap_or_default()
                .prepend_messages(vec![UserMessage::from(text)]),
        );
    }

    pub(crate) fn same_pending_input(&self, other: &Self) -> bool {
        // Server snapshots may refresh instruction templates and lifecycle bookkeeping
        // without changing the human's pending input or selected model/mode controls.
        self.composer == other.composer
            && self.questions == other.questions
            && self.input_queue == other.input_queue
            && self.current_collaboration_mode.mode == other.current_collaboration_mode.mode
            && self.current_collaboration_mode.settings.model
                == other.current_collaboration_mode.settings.model
            && self.current_collaboration_mode.settings.reasoning_effort
                == other.current_collaboration_mode.settings.reasoning_effort
            && self
                .active_collaboration_mask
                .as_ref()
                .map(|mask| (&mask.mode, &mask.model, &mask.reasoning_effort))
                == other
                    .active_collaboration_mask
                    .as_ref()
                    .map(|mask| (&mask.mode, &mask.model, &mask.reasoning_effort))
            && self.plan_mode_reasoning_effort == other.plan_mode_reasoning_effort
    }
}

impl ChatWidget {
    pub(crate) fn mention_usage_mut(&mut self) -> &mut MentionUsage {
        self.bottom_pane.mention_usage_mut()
    }

    pub(crate) fn take_kill_buffer_snapshot(&mut self) -> KillBufferSnapshot {
        self.bottom_pane.take_kill_buffer_snapshot()
    }

    pub(crate) fn restore_kill_buffer_snapshot(&mut self, snapshot: KillBufferSnapshot) {
        self.bottom_pane.restore_kill_buffer_snapshot(snapshot);
    }

    #[cfg(test)]
    pub(crate) fn composer_draft_snapshot(&mut self) -> ComposerDraftSnapshot {
        self.bottom_pane.flush_pending_composer_input();
        let mut draft = self.bottom_pane.composer_draft_snapshot();
        // This snapshot comes from an active host, where disabled shell commands
        // also mean a leading bang was entered literally.
        draft.shell_mode = Some(draft.shell_mode.unwrap_or(false));
        draft
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
        let startup_undo = (startup_has_content && !self.local_settings.tui.vim_mode_default)
            .then(|| self.bottom_pane.startup_undo_draft())
            .flatten();

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
            self.restore_composer_state_with_shell_mode(
                Self::composer_state_from_user_message(
                    merge_user_messages(vec![existing_message, startup_message]),
                    pending_pastes,
                ),
                existing_draft.shell_mode,
            );
            startup_offset
                .saturating_add(draft.cursor)
                .saturating_add(cursor_adjustment)
        } else if existing_has_content || !startup_has_content {
            existing_cursor
        } else {
            self.restore_composer_state_with_shell_mode(
                Self::composer_state_from_user_message(startup_message, draft.pending_pastes),
                draft.shell_mode,
            );
            draft.cursor
        };
        if let Some(destination) = startup_undo {
            self.bottom_pane.record_startup_undo(destination);
        }
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
        if self.suppress_initial_user_message_submit || self.submission_settings_pending() {
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
            // Command-line input bypasses the composer, which normally records local recall.
            let mut entry = HistoryEntry {
                text: user_message.text.clone(),
                text_elements: user_message.text_elements.clone(),
                local_image_paths: user_message
                    .local_images
                    .iter()
                    .map(|image| image.path.clone())
                    .collect(),
                remote_image_urls: user_message.remote_image_urls.clone(),
                mention_bindings: user_message.mention_bindings.clone(),
                pending_pastes: Vec::new(),
            };
            // CLI images have no composer elements. Give recalled attachments owned placeholders
            // so the composer retains them when the user submits the recalled prompt again.
            for index in 0..entry.local_image_paths.len() {
                let placeholder = local_image_label_text(entry.remote_image_urls.len() + index + 1);
                if entry
                    .text_elements
                    .iter()
                    .any(|element| element.placeholder(&entry.text) == Some(placeholder.as_str()))
                {
                    continue;
                }
                if !entry.text.is_empty() {
                    entry.text.push(' ');
                }
                let start = entry.text.len();
                entry.text.push_str(&placeholder);
                entry.text_elements.push(TextElement::new(
                    (start..entry.text.len()).into(),
                    Some(placeholder),
                ));
            }
            if self.submit_user_message_with_history_record(
                user_message,
                UserMessageHistoryRecord::UserMessageText,
            ) {
                self.bottom_pane.record_local_user_message_history(entry);
            }
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
        self.enqueue_rejected_steer_by_id(None)
    }

    pub(super) fn enqueue_rejected_steer_by_id(&mut self, client_id: Option<&str>) -> bool {
        let pending = match client_id {
            Some(id) => self.take_submitted_message(id),
            None => self
                .input_queue
                .pending_steer
                .take()
                .map(|steer| (steer.user_message, steer.history_record, steer.source)),
        };
        let Some((message, history_record, source)) = pending else {
            return false;
        };
        self.input_queue
            .rejected_steers_queue
            .push_back(QueuedUserMessage {
                source,
                history_record,
                ..message.into()
            });
        self.refresh_pending_input_preview();
        self.update_task_running_state();
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

    /// Handle a turn aborted due to user interrupt, budget exhaustion,
    /// or review completion.
    /// When there are queued user messages, restore them into the composer
    /// separated by newlines rather than auto-submitting the next one.
    pub(super) fn on_interrupted_server_turn(&mut self, reason: TurnAbortReason, turn_id: &str) {
        let later = if self
            .input_queue
            .pending_steer
            .as_ref()
            .is_some_and(|pending| pending.admission.turn_id().is_some_and(|id| id != turn_id))
        {
            std::mem::take(&mut self.input_queue.pending_steer)
        } else {
            Default::default()
        };
        let mut unconfirmed = VecDeque::new();
        self.input_queue.queued_user_messages.retain(|message| {
            if matches!(message.delivery, MessageDelivery::Unconfirmed(_)) {
                unconfirmed.push_back(message.clone());
                false
            } else {
                true
            }
        });
        self.on_interrupted_turn(reason);
        if later.is_some() {
            self.input_queue.pending_steer = later;
        }
        self.input_queue.queued_user_messages.extend(unconfirmed);
        self.refresh_pending_input_preview();
    }

    pub(super) fn on_interrupted_turn(&mut self, reason: TurnAbortReason) {
        self.requeue_image_submission();
        // Finalize, log a gentle prompt, and clear running state.
        self.finalize_turn();
        self.rest_after_turn(crate::bottom_pane::Rest::Interrupted);
        // Cancellation means idle in the program-status protocol, not a failed task.
        if reason == TurnAbortReason::Interrupted {
            self.status_state.program_status_after_turn =
                crate::terminal_program_status::ProgramStatus::Idle;
        }
        let send_pending_steer_immediately = self.input_queue.pending_steer.interrupt()
            == input_queue::SteerInterrupt::SubmitAsTurn
            && !self.is_read_only_view();
        self.input_queue
            .pending_steer
            .set_interrupt(input_queue::SteerInterrupt::RestoreDraft);
        if !self.is_archived_view() {
            self.input_queue.pending_steer.finish_replacement(true);
        }
        if self.interrupted_turn_notice_mode != InterruptedTurnNoticeMode::Suppress {
            if send_pending_steer_immediately {
                self.add_to_history(history_cell::new_info_event(
                    "Model interrupted to submit steer instructions.".to_owned(),
                    /*hint*/ None,
                ));
            } else {
                match reason {
                    TurnAbortReason::BudgetLimited => {
                        self.add_to_history(history_cell::new_error_event(
                            "Goal budget reached - the turn was stopped.".to_string(),
                        ))
                    }
                    TurnAbortReason::Interrupted => {
                        self.add_to_history(history_cell::InterruptionNotice)
                    }
                }
            }
        }

        // Preserve the queue and its receipt identities until the archived view is reattached.
        if self.is_archived_view() {
            self.refresh_pending_input_preview();
            self.request_redraw();
            return;
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
            shell_mode: Some(composer.shell_mode.unwrap_or(false)),
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
            shell_mode: Some(draft.shell_mode.unwrap_or(false)),
            text_elements: draft.text_elements,
            local_images: draft.local_images,
            remote_image_urls: draft.remote_image_urls,
            mention_bindings: draft.mention_bindings,
            pending_pastes: draft.pending_pastes,
        };
        self.restore_composer_state(composer.prepend_messages(vec![user_message]));
    }

    pub(super) fn restore_composer_state(&mut self, composer: ThreadComposerState) {
        let shell_mode = composer.shell_mode;
        self.restore_composer_state_with_shell_mode(composer, shell_mode);
    }

    fn restore_composer_state_with_shell_mode(
        &mut self,
        composer: ThreadComposerState,
        shell_mode: Option<bool>,
    ) {
        let ThreadComposerState {
            text,
            shell_mode: _,
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
            if shell_mode == Some(false) {
                ShellPrefixHandling::Literal
            } else {
                ShellPrefixHandling::Interpret
            },
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
            shell_mode: Some(false),
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
            // A disabled shell composer also treats a leading bang literally.
            shell_mode: Some(draft.shell_mode.unwrap_or(false)),
            text_elements: draft.text_elements,
            local_images: draft.local_images,
            remote_image_urls: draft.remote_image_urls,
            mention_bindings: draft.mention_bindings,
            pending_pastes: draft.pending_pastes,
        };
        Some(ThreadInputState {
            provider_error: self.provider_error.clone(),
            questions: self
                .bottom_pane
                .questions
                .as_deref_mut()
                .map(crate::bottom_pane::AsyncQuestions::capture),
            pending_thread_settings: None,
            initial_user_message: self.initial_user_message.clone(),
            composer: composer.has_content().then_some(composer),
            input_queue: self.input_queue.clone(),
            reconnect_pending: false,
            current_collaboration_mode: self.current_collaboration_mode.clone(),
            active_collaboration_mask: self.active_collaboration_mask.clone(),
            plan_mode_reasoning_effort: self.config.plan_mode_reasoning_effort.clone(),
            status_surface_overrides: self.status_surface_overrides.clone(),
            task_running: self.bottom_pane.is_task_running(),
            agent_turn_running: self.turn_lifecycle.agent_turn_running,
        })
    }

    /// Restore local input without replacing settings already accepted for this session.
    pub(crate) fn restore_bound_thread_input_state(
        &mut self,
        mut input_state: Option<ThreadInputState>,
        restore_mode: ThreadInputStateRestoreMode,
        authority: ThreadInputSettingsAuthority,
    ) {
        let ThreadInputSettingsAuthority::Session { accepted_mode } = authority else {
            self.restore_thread_input_state(input_state, restore_mode);
            return;
        };
        if let Some(input) = input_state.as_mut() {
            input.current_collaboration_mode = self.current_collaboration_mode.clone();
            if let Some(mask) = input.active_collaboration_mask.as_mut() {
                // Absence inherits the confirmed base instead of duplicating it.
                mask.model = None;
                mask.reasoning_effort = None;
            }
        }
        self.restore_thread_input_state(input_state, restore_mode);
        if let Some(mode) = accepted_mode {
            self.set_effective_collaboration_mode(mode);
        }
    }

    pub(crate) fn restore_thread_input_state(
        &mut self,
        input_state: Option<ThreadInputState>,
        restore_mode: ThreadInputStateRestoreMode,
    ) {
        let provider_error = input_state
            .as_ref()
            .and_then(|state| state.provider_error.clone());
        let preserve_in_flight_turn = restore_mode.preserve_in_flight_turn;
        let restored_task_running = preserve_in_flight_turn
            && (self.bottom_pane.is_task_running()
                || input_state.as_ref().is_some_and(|state| state.task_running));
        let restored_agent_turn_running = preserve_in_flight_turn
            && (self.turn_lifecycle.agent_turn_running
                || input_state
                    .as_ref()
                    .is_some_and(|state| state.agent_turn_running));
        if let Some(input_state) = input_state {
            self.bottom_pane.restore_questions(input_state.questions);
            self.initial_user_message = input_state.initial_user_message;
            self.current_collaboration_mode = input_state.current_collaboration_mode;
            self.active_collaboration_mask = input_state.active_collaboration_mask;
            self.config.plan_mode_reasoning_effort = input_state.plan_mode_reasoning_effort;
            self.restore_status_surface_overrides(input_state.status_surface_overrides);
            self.turn_lifecycle
                .restore_running(restored_agent_turn_running, Instant::now());
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
        if let Some(error) = provider_error {
            self.show_provider_error(error, /*rejected_message_id*/ None);
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
                .chain(std::iter::once(self)),
        )
    }

    pub(super) fn merge(states: impl IntoIterator<Item = Self>) -> Self {
        let mut states = states.into_iter().filter(Self::has_content);
        let first = states.next().unwrap_or_default();
        let Some(second) = states.next() else {
            return first;
        };
        let shell_mode = first.shell_mode;
        let mut used = HashSet::new();
        let mut messages = Vec::new();
        let mut pastes = Vec::new();
        for state in [first, second].into_iter().chain(states) {
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
        ThreadComposerState {
            shell_mode,
            ..ChatWidget::composer_state_from_user_message(merge_user_messages(messages), pastes)
        }
    }
}

impl InputQueueState {
    pub(super) fn pop_next(&mut self) -> Option<QueuedUserMessage> {
        if self.rejected_steers_queue.is_empty() {
            return self.queued_user_messages.pop_front();
        }
        let dispatch = self.rejected_steers_queue.front()?.dispatch;
        let same_route = self
            .rejected_steers_queue
            .iter()
            .take_while(|message| message.dispatch == dispatch)
            .count();
        let count = self
            .rejected_steers_queue
            .iter()
            .position(|message| crate::async_question_reply::parse(&message.text).is_some())
            .map(|index| index.max(1))
            .unwrap_or(self.rejected_steers_queue.len())
            .min(same_route);
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
            dispatch,
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
                delivery: if uncertain || steer.admission.turn_id().is_some() {
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
