//! User input submission, queue draining, and draft restore flow for `ChatWidget`.
//!
//! The queue data itself lives in `input_queue`; this module owns the app-level
//! effects around taking composer input, submitting user turns, draining queued
//! follow-ups, and restoring draft state across interrupts or thread switches.
//! Composer submissions resume transcript following before dispatch or startup queueing,
//! except for reversible settings pickers that preserve the reading position.

use super::submission_dispatch::UserTurnDispatch;
use super::*;

pub(super) struct PreparedSubagentText {
    thread_id: ThreadId,
    expected_turn_id: Option<String>,
    source: UserMessageSource,
    input: Vec<codex_app_server_protocol::UserInput>,
}

impl ChatWidget {
    pub(crate) fn is_parent_owned_thread(&self) -> bool {
        self.blocks_direct_input && !self.active_side_conversation
    }

    pub(crate) fn set_parent_owned_thread(&mut self) {
        self.cancel_image_submission();
        self.blocks_direct_input = true;
        self.bottom_pane.set_parent_owned_thread();
    }

    pub(crate) fn set_child_text_input_enabled(&mut self) {
        self.bottom_pane.set_child_text_input_enabled();
    }

    pub(super) fn submit_subagent_text(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        source: UserMessageSource,
    ) -> bool {
        if self.external_writer_view || self.has_misalignment_policy_violation() {
            return false;
        }
        match self.prepare_subagent_text(text, text_elements, source) {
            Ok(input) => {
                self.emit_subagent_text(input);
                true
            }
            Err(message) => {
                self.add_error_message(message.into());
                false
            }
        }
    }

    pub(super) fn prepare_subagent_text(
        &self,
        text: String,
        text_elements: Vec<TextElement>,
        source: UserMessageSource,
    ) -> Result<PreparedSubagentText, &'static str> {
        if self.turn_lifecycle.agent_turn_running && self.turn_lifecycle.last_turn_id.is_none() {
            return Err("Turn is not ready; send again");
        }
        let Some(thread_id) = self.thread_id else {
            return Err("Thread is not ready; send again");
        };
        Ok(PreparedSubagentText {
            thread_id,
            source,
            expected_turn_id: self
                .turn_lifecycle
                .agent_turn_running
                .then(|| self.turn_lifecycle.last_turn_id.clone())
                .flatten(),
            input: vec![codex_app_server_protocol::UserInput::Text {
                text,
                text_elements: text_elements.into_iter().map(Into::into).collect(),
            }],
        })
    }

    #[expect(
        clippy::expect_used,
        reason = "A selected child always has capturable input state."
    )]
    pub(super) fn emit_subagent_text(&mut self, input: PreparedSubagentText) {
        let source = match input.source {
            UserMessageSource::Prompt | UserMessageSource::SafetyRecourse => {
                crate::app::turn_submissions::SubagentTextSource::Prompt
            }
            UserMessageSource::QuestionAnswer => {
                crate::app::turn_submissions::SubagentTextSource::QuestionAnswer(Box::new(
                    self.capture_thread_input_state()
                        .expect("selected child input state"),
                ))
            }
        };
        self.app_event_tx.send(AppEvent::SubagentTextInput {
            thread_id: input.thread_id,
            expected_turn_id: input.expected_turn_id,
            source,
            input: input.input,
        });
    }

    pub(super) fn handle_composer_input_result(
        &mut self,
        input_result: InputResult,
        had_modal_or_popup: bool,
    ) {
        let follow_transcript = match &input_result {
            // Opening settings is not a request to leave the current reading anchor.
            // Inline commands still follow so their output (including usage errors) is visible.
            InputResult::Command(
                SlashCommand::Model
                | SlashCommand::Presets
                | SlashCommand::Keymap
                | SlashCommand::Memories
                | SlashCommand::Config
                | SlashCommand::Title
                | SlashCommand::Statusline
                | SlashCommand::Theme,
            ) => false,
            InputResult::Command(_)
            | InputResult::ServiceTierCommand(_)
            | InputResult::CommandWithArgs(..) => true,
            InputResult::LocalShell(_)
            | InputResult::Submitted { .. }
            | InputResult::Queued { .. }
            | InputResult::ParentOwnedInputBlocked
            | InputResult::None
            | InputResult::NavigationDownBoundary => false,
        };
        if follow_transcript {
            self.app_event_tx.send(AppEvent::FollowTranscript);
        }
        match input_result {
            InputResult::LocalShell(request) => {
                self.app_event_tx.send(AppEvent::LocalShellRequest(request));
            }
            InputResult::NavigationDownBoundary => {
                self.subagent_roster
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .down();
                self.request_redraw();
            }
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                if self.is_parent_owned_thread() {
                    self.submit_subagent_text(text, text_elements, UserMessageSource::Prompt);
                    return;
                }
                let user_message = self.user_message_from_submission(text, text_elements);
                if user_message.text.is_empty()
                    && user_message.local_images.is_empty()
                    && user_message.remote_image_urls.is_empty()
                {
                    return;
                }
                self.app_event_tx.send(AppEvent::FollowTranscript);
                let should_submit_now = self.is_session_configured()
                    && !self.is_plan_streaming_in_tui()
                    && !self.input_queue_frontend.submissions_paused()
                    && !self.submission_settings_pending()
                    && (!self.input_queue.turn.is_pending()
                        || self.turn_lifecycle.agent_turn_running);
                if should_submit_now {
                    if self.only_user_shell_commands_running()
                        && !user_message.text.starts_with('!')
                    {
                        self.queue_user_message(user_message);
                        return;
                    }
                    // Submitted is emitted when user submits.
                    // Reset any reasoning header only when we are actually submitting a turn.
                    self.reasoning_buffer.clear();
                    self.reasoning_header = None;
                    self.reasoning_summary_parts.clear();
                    self.set_status_header(String::from("Working"));
                    self.submit_user_message_with_history_and_shell_escape_policy(
                        user_message,
                        UserMessageHistoryRecord::UserMessageText,
                        ShellEscapePolicy::Allow,
                        UserMessageSource::Prompt,
                    );
                } else {
                    self.queue_user_message(user_message);
                }
            }
            InputResult::Queued {
                text,
                text_elements,
                action,
                pending_pastes,
            } => {
                let user_message = self.user_message_from_submission(text, text_elements);
                if self.queue_user_message_with_options(user_message, action, pending_pastes) {
                    self.app_event_tx.send(AppEvent::FollowTranscript);
                }
            }
            InputResult::Command(cmd) => {
                self.handle_slash_command_dispatch(cmd);
                // A settings command can instead report why it is unavailable.
                if !follow_transcript && self.bottom_pane.no_modal_or_popup_active() {
                    self.app_event_tx.send(AppEvent::FollowTranscript);
                }
            }
            InputResult::ServiceTierCommand(command) => {
                self.handle_service_tier_command_dispatch(command);
            }
            InputResult::CommandWithArgs(cmd, args, text_elements) => {
                self.handle_slash_command_with_args_dispatch(cmd, args, text_elements);
            }
            InputResult::ParentOwnedInputBlocked => {
                self.add_error_message(PARENT_OWNED_INPUT_MESSAGE.to_string());
            }
            InputResult::None => {}
        }
        if had_modal_or_popup && self.bottom_pane.no_modal_or_popup_active() {
            self.maybe_send_next_queued_input();
        }
    }

    pub(super) fn defer_input_until_settings_applied(&mut self) {
        if !self.bottom_pane.no_modal_or_popup_active() {
            self.input_queue_frontend.suppress_queue_autosend = true;
        }
    }

    pub(super) fn on_modal_or_popup_closed(&mut self) {
        if self.apply_pending_permission_profile() {
            return;
        }
        if self.cancel_pending_permission_profile_selection() {
            return;
        }
        self.close_permission_menu();
        if self.input_queue_frontend.suppress_queue_autosend {
            self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
        } else {
            self.maybe_send_next_queued_input();
        }
    }

    pub(super) fn queue_user_message(&mut self, user_message: UserMessage) -> bool {
        self.queue_user_message_with_options_and_source(
            user_message,
            QueuedInputAction::Plain,
            Vec::new(),
            UserMessageSource::Prompt,
        )
    }

    pub(crate) fn set_queue_submissions_until_session_configured(&mut self, queue: bool) {
        self.bottom_pane
            .set_queue_submissions(queue && !self.is_session_configured());
    }

    pub(crate) fn queue_user_message_with_options(
        &mut self,
        user_message: UserMessage,
        action: QueuedInputAction,
        pending_pastes: Vec<(String, String)>,
    ) -> bool {
        self.queue_user_message_with_options_and_source(
            user_message,
            action,
            pending_pastes,
            UserMessageSource::Prompt,
        )
    }

    pub(super) fn queue_user_message_with_options_and_source(
        &mut self,
        user_message: UserMessage,
        action: QueuedInputAction,
        pending_pastes: Vec<(String, String)>,
        source: UserMessageSource,
    ) -> bool {
        if self.has_misalignment_policy_violation() {
            return false;
        }
        let should_run_now = self.is_session_configured()
            && !self.is_user_turn_pending_or_running()
            && !self.input_queue_frontend.submissions_paused()
            && !self.submission_settings_pending();
        if action != QueuedInputAction::ParseSlash {
            self.empty_state_animation.borrow_mut().dismiss();
        }
        if !should_run_now || action != QueuedInputAction::Plain {
            self.input_queue
                .queued_user_messages
                .push_back(QueuedUserMessage {
                    user_message,
                    prepared_images: None,
                    action,
                    delivery: MessageDelivery::Unsent,
                    pending_pastes,
                    source,
                    dispatch: UserTurnDispatch::Frontend.capture(self.thread_id),
                    history_record: UserMessageHistoryRecord::UserMessageText,
                });
            self.refresh_pending_input_preview();
            if should_run_now
                && !(self.is_parent_owned_thread() && source == UserMessageSource::QuestionAnswer)
            {
                self.maybe_send_next_queued_input();
            }
            self.request_draft_session();
            true
        } else {
            self.submit_user_message_with_history_and_shell_escape_policy(
                user_message,
                UserMessageHistoryRecord::UserMessageText,
                ShellEscapePolicy::Allow,
                source,
            )
            .0
        }
    }

    /// If idle and there are queued inputs, submit exactly one to start the next turn.
    pub(crate) fn maybe_send_next_queued_input(&mut self) -> bool {
        if !self.is_session_configured()
            || self.fork_in_progress
            || self.has_misalignment_policy_violation()
            || self.input_queue_frontend.submissions_paused()
            || self.submission_settings_pending()
            || self.input_queue.recovered_queue
            || self.input_queue.has_unconfirmed_messages()
        {
            return false;
        }
        if self.blocks_direct_input
            && !(self.is_parent_owned_thread()
                && self
                    .input_queue
                    .queued_user_messages
                    .front()
                    .is_some_and(|message| message.source == UserMessageSource::QuestionAnswer))
        {
            return false;
        }
        if self.is_user_turn_pending_or_running() {
            return false;
        }
        if self.is_parent_owned_thread() {
            let Some(message) = self.input_queue.queued_user_messages.pop_front() else {
                return false;
            };
            let submitted = self.submit_subagent_text(
                message.user_message.text.clone(),
                message.user_message.text_elements.clone(),
                message.source,
            );
            if !submitted {
                self.input_queue.queued_user_messages.push_front(message);
            }
            self.refresh_pending_input_preview();
            self.request_draft_session();
            return submitted;
        }
        let mut submitted_follow_up = false;
        while !self.is_user_turn_pending_or_running() {
            let Some((queued_message, history_record)) = self.pop_next_queued_user_message() else {
                break;
            };
            match queued_message.action {
                QueuedInputAction::Plain => {
                    let source = queued_message.source;
                    let dispatch = queued_message.dispatch;
                    submitted_follow_up = self
                        .submit_user_message_with_dispatch(
                            queued_message.into_user_message(),
                            history_record,
                            ShellEscapePolicy::Allow,
                            source,
                            None,
                            dispatch,
                        )
                        .0;
                    break;
                }
                QueuedInputAction::Literal => {
                    let QueuedUserMessage {
                        user_message,
                        prepared_images,
                        pending_pastes,
                        source,
                        dispatch,
                        ..
                    } = queued_message;
                    let mut restored_pending_pastes = self.bottom_pane.composer_pending_pastes();
                    let mut used_paste_placeholders = restored_pending_pastes
                        .iter()
                        .map(|(placeholder, _)| placeholder.clone())
                        .collect();
                    let (mut user_message, pending_pastes) =
                        super::user_messages::remap_colliding_paste_placeholders(
                            user_message,
                            pending_pastes,
                            &mut used_paste_placeholders,
                        );
                    if !self.current_model().trim().is_empty()
                        && (self.current_model_supports_images()
                            || (user_message.local_images.is_empty()
                                && user_message.remote_image_urls.is_empty()))
                    {
                        (user_message.text, user_message.text_elements) =
                            crate::bottom_pane::ChatComposer::expand_pending_pastes(
                                &user_message.text,
                                user_message.text_elements,
                                &pending_pastes,
                            );
                    }
                    submitted_follow_up = self
                        .submit_user_message_with_dispatch(
                            user_message,
                            history_record,
                            ShellEscapePolicy::Disallow,
                            source,
                            prepared_images,
                            dispatch,
                        )
                        .0;
                    if !submitted_follow_up {
                        restored_pending_pastes.extend(pending_pastes);
                        self.bottom_pane
                            .set_composer_pending_pastes(restored_pending_pastes);
                    }
                    break;
                }
                QueuedInputAction::ParseSlash => {
                    let drain = self.submit_queued_slash_prompt(queued_message);
                    if drain == QueueDrain::Stop {
                        submitted_follow_up = self.is_user_turn_pending_or_running();
                        break;
                    }
                }
                QueuedInputAction::RunShell => {
                    let drain = self.submit_queued_shell_prompt(queued_message.into_user_message());
                    if drain == QueueDrain::Stop {
                        submitted_follow_up = self.is_user_turn_pending_or_running();
                        break;
                    }
                }
            }
        }
        // Update the list to reflect the remaining queued messages (if any).
        self.refresh_pending_input_preview();
        submitted_follow_up
    }

    pub(crate) fn is_user_turn_pending_or_running(&self) -> bool {
        self.pending_image_submission.is_some()
            || self.input_queue.turn.is_pending()
            || self.turn_lifecycle.is_running_or_admitted()
            || self.review.is_review_mode
            || (self.bottom_pane.is_task_running() && self.mcp_startup_status.is_none())
    }

    pub(super) fn only_user_shell_commands_running(&self) -> bool {
        self.turn_lifecycle.agent_turn_running
            && !self.running_commands.is_empty()
            && self
                .running_commands
                .values()
                .all(|command| command.source == ExecCommandSource::UserShell)
    }

    /// Rebuild and update the bottom-pane pending-input preview.
    pub(super) fn refresh_pending_input_preview(&mut self) {
        let has_queued = self.has_queued_follow_up_messages()
            || !self.input_queue_frontend.server_messages.is_empty()
            || self.has_waiting_submission();
        if let Some(questions) = &mut self.bottom_pane.questions {
            questions.has_queued_messages = has_queued;
        }
        let mut preview = self.input_queue.preview();
        preview
            .queued_messages
            .splice(0..0, self.submission_previews());
        preview
            .queued_messages
            .splice(0..0, self.server_queue_previews());
        if let Some(pending) = &self.pending_image_submission {
            preview.queued_messages.insert(
                /*index*/ 0,
                format!("Preparing images: {}", pending.message.text),
            );
        }
        self.bottom_pane.set_pending_input_preview(
            preview.queued_messages,
            preview.pending_steer,
            preview.rejected_steers,
        );
    }

    pub(crate) fn submit_user_message_with_mode(
        &mut self,
        text: String,
        mut collaboration_mode: CollaborationModeMask,
    ) {
        if self.blocks_direct_input {
            self.add_error_message(if self.external_writer_view {
                "This thread is open elsewhere. Close it there and retry resume to continue."
                    .to_string()
            } else {
                PARENT_OWNED_INPUT_MESSAGE.to_string()
            });
            return;
        }
        if collaboration_mode.mode == Some(ModeKind::Plan)
            && let Some(effort) = self.config.plan_mode_reasoning_effort.clone()
        {
            collaboration_mode.reasoning_effort = Some(Some(effort));
        }
        if self.turn_lifecycle.agent_turn_running
            && self.active_collaboration_mask.as_ref() != Some(&collaboration_mode)
        {
            self.add_error_message(
                "Cannot switch collaboration mode while a turn is running.".to_string(),
            );
            return;
        }
        self.set_collaboration_mask_from_user_action(collaboration_mode);
        let should_queue = self.is_plan_streaming_in_tui();
        let user_message = UserMessage {
            text,
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        };
        if should_queue {
            self.queue_user_message(user_message);
        } else {
            self.submit_user_message(user_message);
        }
    }

    #[cfg(test)]
    pub(crate) fn queued_user_message_texts(&self) -> Vec<String> {
        self.input_queue
            .rejected_steers_queue
            .iter()
            .map(|message| message.text.clone())
            .chain(
                self.input_queue
                    .queued_user_messages
                    .iter()
                    .map(|message| message.text.clone()),
            )
            .collect()
    }
}
