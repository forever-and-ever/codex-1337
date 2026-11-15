//! AppEvent dispatch for the TUI app.
//!
//! This module contains the exhaustive `AppEvent` dispatcher and exit-mode handling. Large domain
//! actions are delegated to focused app submodules so the central match remains the routing layer.

use super::rate_limit_refresh::RateLimitReadStatus;
use super::rate_limit_refresh::RateLimitRefreshOutcome;
use super::resize_reflow::trailing_run_start;
mod continuations;

use super::*;
use crate::app_event::AgentsOverviewAction;
use crate::app_event::RecapTrigger;
use crate::app_event::ThreadTitleDestination;
use crate::app_server_session::turn_permissions_overrides;
use crate::config_update::format_config_error;
use crate::pager_overlay::TranscriptHistoryState;
use crate::session_resume::cwds_differ;
use crate::status::StatusAccountDisplay;
use codex_app_server_protocol::ThreadGoalStatus;
#[cfg(target_os = "windows")]
use codex_app_server_protocol::WindowsSandboxSetupMode;

pub(super) const SHUTDOWN_FIRST_EXIT_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 2);

impl App {
    // Keep the existing dispatcher layout until it can be formatted separately.
    #[rustfmt::skip]
    pub(crate) async fn handle_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: AppEvent,
    ) -> Result<AppRunControl> {
        let Some(event) = self.consume_local_shell_event(event, tui) else {
            return Ok(AppRunControl::Continue);
        };
        // Release the shortcut's input guard even when a fork is rejected below.
        if matches!(event, AppEvent::ForkCurrentSession { .. }) {
            self.chat_widget.fork_in_progress = false;
        }
        if self.reconnect.offline && matches!(event, AppEvent::Exit(ExitMode::ShutdownFirst)) {
            return Ok(AppRunControl::Exit(ExitReason::UserRequested));
        }
        if self.reconnect.offline
            && !matches!(
                &event,
                AppEvent::OpenDaemonMenu
                    | AppEvent::RefreshClaudeQuota { .. }
                    | AppEvent::ClaudeQuotaLoaded { .. }
                    | AppEvent::JiraFinished { .. }
                    | AppEvent::FarmFinished { .. }
                    | AppEvent::RestoreFarmReceipt { .. }
                    | AppEvent::OpenWarnings
                    | AppEvent::Queue(_)
                    | AppEvent::Submission(_)
                    | AppEvent::StatusLineSetup { .. }
                    | AppEvent::StatusLineSetupCancelled
                    | AppEvent::TerminalTitleSetup { .. }
                    | AppEvent::TerminalTitleSetupPreview { .. }
                    | AppEvent::TerminalTitleSetupCancelled
                    | AppEvent::OpenConfigEditor(_)
                    | AppEvent::Provider(_)
                    | AppEvent::SyntaxThemeSelected { .. }
                    | AppEvent::SyntaxThemeFollowConfig
                    | AppEvent::SyntaxThemeFollowConfigPreview
                    | AppEvent::ResolveTheme
                    | AppEvent::SyntaxThemePreviewed
                    | AppEvent::OpenKeymapActionMenu { .. }
                    | AppEvent::OpenKeymapReplaceBindingMenu { .. }
                    | AppEvent::OpenKeymapCapture { .. }
                    | AppEvent::OpenKeymapDebug
                    | AppEvent::KeymapCaptured { .. }
                    | AppEvent::KeymapCleared { .. }
                    | AppEvent::TestConfigNotification
                    | AppEvent::SendConfigTestNotification { .. }
                    | AppEvent::SaveConfigPreference(_)
                    | AppEvent::CopyWarning(_)
                    | AppEvent::UpdateWarnings { .. }
                    | AppEvent::CopySelection { .. }
                    | AppEvent::SelectTranscriptCopy { .. }
                    | AppEvent::TranscriptCopyClosed
                    | AppEvent::InsertHistoryCell(_)
                    | AppEvent::DiscardStreamItems(_)
                    | AppEvent::CodeModeTranscript { .. }
                    | AppEvent::CodeModeProjection { .. }
                    | AppEvent::RecordToolOutputTokens { .. }
                    | AppEvent::RecordStepAdmission { .. }
                    | AppEvent::RecordFinalResponse { .. }
                    | AppEvent::CommitRealtimeTranscriptHistory
                    | AppEvent::ParkSideCompanion { .. }
                    | AppEvent::SideChatRemovalFinished { .. }
                    | AppEvent::SideChatArchived { .. }
                    | AppEvent::SavedChatPrepared { .. } | AppEvent::SavedChatReleased { .. }
                    | AppEvent::StartQueuedSavedChat
                    | AppEvent::SavedSideChatsLoaded { .. }
                    | AppEvent::SideReplayFinished(_)
                    | AppEvent::ResetTranscriptForThreadSwitch
                    | AppEvent::ResetTranscriptForThreadSwitchPreservingScreen
                    | AppEvent::FinishPromptRevert { .. }
                    | AppEvent::ReconcilePromptRevert { .. }
                    | AppEvent::PromptRevertPrepared { .. }
                    | AppEvent::PromptRevertFinished { .. }
                    | AppEvent::TranscriptExportProgress { .. }
                    | AppEvent::FinishTranscriptExport { .. }
                    | AppEvent::CancelTranscriptExport
                    | AppEvent::AgentsOverviewPinToggled { .. }
                    | AppEvent::AppendMessageHistoryEntry { .. }
                    | AppEvent::BeginInitialHistoryReplayBuffer
                    | AppEvent::BeginThreadSwitchHistoryReplayBuffer
                    | AppEvent::EndInitialHistoryReplayBuffer
                    | AppEvent::FatalExitRequest(_)
            )
        {
            return Ok(AppRunControl::Continue);
        }
        if matches!(
            &event,
            AppEvent::OpenWindowsSandboxEnablePrompt { .. }
                | AppEvent::OpenWindowsSandboxFallbackPrompt { .. }
                | AppEvent::BeginWindowsSandboxElevatedSetup { .. }
                | AppEvent::BeginWindowsSandboxLegacySetup { .. }
                | AppEvent::EnableWindowsSandboxForAgentMode { .. }
        ) && !self.windows_sandbox_setup_is_local()
        {
            if matches!(
                &event,
                AppEvent::OpenWindowsSandboxFallbackPrompt { .. }
                    | AppEvent::EnableWindowsSandboxForAgentMode { .. }
            ) {
                #[cfg(any(target_os = "windows", test))]
                {
                    self.windows_sandbox.pending_setup = None;
                }
                self.chat_widget.clear_windows_sandbox_setup_status();
            }
            self.chat_widget.add_info_message(
                "Windows sandbox setup requires local connections and executors.".to_string(),
                /*hint*/ None,
            );
            return Ok(AppRunControl::Continue);
        }
        if self.chat_widget.has_misalignment_policy_violation()
            && matches!(
                event,
                AppEvent::OpenAgentPicker
                    | AppEvent::Farm { .. }
                    | AppEvent::SelectAgentThread(_)
                    | AppEvent::StartSide { .. }
                    | AppEvent::ForkCurrentSession { .. }
                    | AppEvent::ConfirmedThreadName {
                        request: super::thread_name::ThreadNameRequest::Fork { .. },
                        ..
                    }
                    | AppEvent::RevertSessionForPromptEdit { .. }
                    | AppEvent::SetThreadGoalDraft { .. }
                    | AppEvent::SetThreadGoalStatus {
                        status: ThreadGoalStatus::Active,
                        ..
                    }
            )
        {
            return Ok(AppRunControl::Continue);
        }

        // Keep the picker check and model update in one event without recursively polling this
        // large dispatcher on the TUI thread's stack.
        let event = match event {
            AppEvent::ScopedModelPicker { backend, thread_id, event } => {
                if backend != app_server.backend_id()
                    || thread_id != self.chat_widget.thread_id()
                    || self.chat_widget.model_catalog().provider_may_have_changed()
                {
                    return Ok(AppRunControl::Continue);
                }
                *event
            }
            event => event,
        };

        let (event, mut sparkle_model) = match event {
            AppEvent::AstraSelectedFromModelPicker {
                thread_id,
                model,
                action,
            } => {
                let should_offer = self.chat_widget.current_model() != model
                    && self.chat_widget.sparkle_thread_for_picker_action(&model) == Some(thread_id);
                let next_event = action.into_app_event(model.clone());
                (next_event, should_offer.then_some(model))
            }
            event => (event, None),
        };
        if matches!(&event,
            AppEvent::OpenAccountPicker { .. }
                | AppEvent::SelectAccount { .. }
                | AppEvent::RenameAccountPrompt { .. }
                | AppEvent::SetAccountNickname { .. }
                | AppEvent::AddClaudeAccount
                | AppEvent::Logout
                | AppEvent::Usage { .. }
                | AppEvent::OpenAnalytics { .. }
                | AppEvent::OpenRateLimitResetCredits
                | AppEvent::OpenRateLimitResetConfirmation { .. }
                | AppEvent::ConsumeRateLimitResetCredit { .. }
                | AppEvent::ProviderModel(super::ProviderModelEvent::Open(_))
                | AppEvent::ProviderModel(super::ProviderModelEvent::Start(_))
                | AppEvent::UpdateReasoningEffort(_)
                | AppEvent::UpdateModel(_)
                | AppEvent::ApplyModelSelection { .. }
                | AppEvent::ApplyAdvancedReasoning { .. }
                | AppEvent::OpenAllModelsPopup
                | AppEvent::FetchModels { .. }
                | AppEvent::PersistModelSelection { .. }
                | AppEvent::SelectSessionModel { .. }
                | AppEvent::ServiceTierSelected { .. }
                | AppEvent::UpdatePlanModeReasoningEffort(_)
                | AppEvent::PersistPlanModeReasoningEffort(_)
        ) && let Some(reason) = self.chat_widget.account_ready_reason() {
            self.chat_widget.add_error_message(reason.into());
            return Ok(AppRunControl::Continue);
        }
        match event {
            AppEvent::DirectoryChangeAdmitted { thread_id, cwd, generation } => {
                if self.navigation_generation() == generation
                    && self.current_displayed_thread_id() == Some(thread_id)
                {
                    self.change_working_directory(tui, app_server, cwd).await;
                }
            }
            AppEvent::VerifiedServerNotification(verified) => {
                self.finish_verified_server_notification(app_server, *verified).await;
            }
            AppEvent::ServerOperationExitReady(reason) => {
                self.supersede_pending_navigation();
                self.drain_local_navigation_recovery().await;
                return Ok(AppRunControl::Exit(reason));
            },
            AppEvent::LocalShellRequest(_) | AppEvent::LocalShellUpdate(_) => {
                unreachable!("Local-shell events are consumed before agent routing")
            }
            AppEvent::OpenDaemonMenu => self.open_daemon_menu(),
            AppEvent::ReloadUi => self.reload_requested = true,
            AppEvent::UserVerificationApproved { thread_id, server_name, request_id } => {
                Box::pin(self.start_user_verification(app_server, thread_id, server_name, request_id)).await?;
            }
            AppEvent::ReviewMisalignment(review) => {
                self.open_misalignment_review(tui, review);
            }
            AppEvent::ContinueSafetyRecourse { thread_id, turn_id, prompt } => {
                self.enqueue_safety_recourse(app_server, thread_id, turn_id, prompt);
            }
            AppEvent::ContinueMisalignment(review) => {
                self.continue_misalignment(app_server, review).await;
            }
            AppEvent::CloseMisalignmentReview => self.chat_widget.show_misalignment_policy_precaution(),
            AppEvent::SkillsListLoaded { ref cwd, .. }
                if cwds_differ(cwd, self.chat_widget.config_ref().cwd.as_path()) =>
            {
                self.skill_load_warnings.startup_complete = true;
            }
            AppEvent::PluginMentionsLoaded { ref cwd, .. }
                if cwds_differ(cwd, self.config.cwd.as_path()) => {}
            AppEvent::NewSession { name } => {
                self.start_fresh_session(
                    tui, app_server, /*session_start_source*/ None,
                    /*initial_user_message*/ None, name,
                )
                .await;
                if self.chat_widget.has_misalignment_policy_violation() {
                    self.chat_widget.show_misalignment_policy_precaution();
                }
            }
            AppEvent::ContinueProviderChat { thread_id } => {
                // A queued shortcut must still refer to the locked chat on screen.
                if self.chat_widget.thread_id() == Some(thread_id)
                    && self.chat_widget.has_provider_error()
                    && let Some(path) = self.chat_widget.rollout_path()
                {
                    let name = self
                        .chat_widget
                        .thread_name()
                        .filter(|name| !name.trim().is_empty())
                        .map_or_else(
                            || "our previous conversation".to_string(),
                            |name| format!("“{name}”"),
                        );
                    let message = format!(
                        "Read {name} and let’s continue where we left off.\n\nSaved conversation: {}",
                        path.display(),
                    );
                    self.enqueue_fresh_draft_navigation(app_server, message.into());
                }
            }
            AppEvent::ChangeWorkingDirectory {
                thread_id,
                requested_cwd,
            } => {
                if self.pending_working_directory_change.is_some()
                    || self.primary_thread_id != Some(thread_id)
                    || !self.chat_widget.can_change_working_directory(thread_id)
                {
                    self.chat_widget.add_error_message(
                        "Changing directories requires an idle primary session without queued input."
                            .to_string(),
                    );
                } else if crate::uses_remote_workspace_or_environment(
                    &self.app_server_target,
                    self.environment_manager.as_ref(),
                ) {
                    self.chat_widget.add_error_message(
                        "Changing directories is not supported for remote workspaces or remote execution environments."
                            .to_string(),
                    );
                } else {
                    let cwd = AbsolutePathBuf::resolve_path_against_base(
                        requested_cwd.as_path(),
                        self.chat_widget.config_ref().cwd.as_path(),
                    );
                    match std::fs::metadata(cwd.as_path()) {
                        Ok(metadata) if metadata.is_dir() => {
                            self.pending_working_directory_change =
                                Some(working_directory::PendingWorkingDirectoryChange {
                                    source_thread_id: thread_id,
                                    source_cwd: self.config.cwd.clone(),
                                    destination: cwd,
                                });
                        }
                        Ok(_) => self
                            .chat_widget
                            .add_error_message(format!("Not a directory: {}", cwd.display())),
                        Err(error) => self.chat_widget.add_error_message(format!(
                            "Cannot access directory {}: {error}",
                            cwd.display()
                        )),
                    }
                }
            }
            AppEvent::StartDraftSession => self.start_draft_session(app_server),
            AppEvent::StartupThreadStarted { backend_id, request_handle, result } => {
                if backend_id != app_server.backend_id() || !self.pending_startup_thread_start {
                    if let Ok(started) = result {
                        self.adopt_navigation_cleanup_on(request_handle, started.session.thread_id);
                    }
                } else {
                    self.handle_startup_thread_started(app_server, result)
                        .await?;
                }
            }
            AppEvent::RequestOlderScrollbackHistory { thread_id } => {
                if self.chat_widget.thread_id() == Some(thread_id)
                    && self.overlay.is_none()
                    && self.scrollback_has_older_history
                {
                    self.request_older_history_page(app_server, thread_id);
                }
            }
            AppEvent::OlderThreadHistoryLoaded {
                thread_id,
                cursor,
                result,
            } => {
                if let Err(err) = self
                    .handle_older_history_page(tui, app_server, thread_id, &cursor, result)
                    .await
                {
                    app_server.cancel_older_history_page(thread_id, &cursor);
                    if self.chat_widget.thread_id() == Some(thread_id) {
                        self.transcript_view.history = TranscriptHistoryState::Failed;
                        if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                            overlay.set_history_state(TranscriptHistoryState::Failed);
                        }
                        tui.frame_requester().schedule_frame();
                    }
                    if let Some(pane) = self.side_companions.get_mut(&thread_id) {
                        pane.view.history = TranscriptHistoryState::Failed;
                        tui.frame_requester().schedule_frame();
                    }
                    tracing::warn!(%thread_id, error = %err, "failed to load older transcript history");
                }
            }
            AppEvent::OpenWarnings => self.chat_widget.open_warnings(&self.transcript_cells),
            AppEvent::Queue(event) => self.handle_queue_event(app_server, event).await,
            AppEvent::Submission(event) => self.dispatch_submission_event(app_server, event).await,

            AppEvent::LocalUiReply(reply) => reply.apply(self, app_server)?,
            AppEvent::UpdateWarnings { transcript, dismissed, kept } => {
                if !Arc::ptr_eq(&transcript, &self.chat_widget.warning_display_state.transcript) {
                    return Ok(AppRunControl::Continue);
                }
                let state = &mut self.chat_widget.warning_display_state.dismissed;
                state.extend(dismissed.into_iter().map(|entry| (entry.id, entry.details)));
                for entry in kept {
                    if state.get(&entry.id) == Some(&entry.details) {
                        state.remove(&entry.id);
                    }
                }
                self.chat_widget.warning_display_state.synced_cells = None;
                tui.frame_requester().schedule_frame();
            }
            AppEvent::CopyWarning(text) => {
                let result = tui.copy_transcript_selection(&text, crate::clipboard_copy::CopyFormat::PlainText);
                self.chat_widget.show_selection_copy_result(result);
            }
            AppEvent::OpenTranscriptExportFilePrompt => {
                self.chat_widget.show_transcript_export_file_prompt();
            }
            AppEvent::OpenTranscriptExportMenu => self.open_transcript_export_menu(),

            AppEvent::CancelTranscriptExport => self.cancel_transcript_export(),
            AppEvent::TranscriptExportProgress { id, pages } => self.transcript_export_progress(id, pages),
            AppEvent::FinishTranscriptExport { id, result } => self.finish_transcript_export(tui, id, result),
            AppEvent::ExportTranscript { destination } => {
                if let Err(error) = self.export_transcript(app_server, destination) {
                    self.chat_widget
                        .add_error_message(format!("Export failed: {error}"));
                }
                if self.chat_widget.no_modal_or_popup_active() {
                    self.chat_widget
                        .set_queue_autosend_suppressed(/*suppressed*/ false);
                    self.chat_widget.maybe_send_next_queued_input();
                }
            }
            AppEvent::TranscriptCopyClosed => {
                self.chat_widget.maybe_send_next_queued_input();
            }
            AppEvent::SelectTranscriptCopy { guard } => {
                self.refresh_owned_transcript(tui)?;
                if !self.transcript_view.begin_copy_mode(&self.transcript_cells, Some(guard)) {
                    self.chat_widget.add_info_message("Nothing to copy".into(), /*hint*/ None);
                }
                tui.frame_requester().schedule_frame();
            }
            AppEvent::CopySelection { text, label, format } => {
                let result = tui.clipboard.copy(text, format, tui.frame_requester());
                self.chat_widget.show_copy_result(&label, result);
            }
            AppEvent::ClearUi { name } => {
                if self.reject_pending_permission_root_switch() {
                    return Ok(AppRunControl::Continue);
                }
                self.clear_terminal_ui(tui, /*redraw_header*/ false)?;
                self.reset_app_ui_state_after_clear();

                self.start_fresh_session(
                    tui,
                    app_server,
                    Some(ThreadStartSource::Clear),
                    /*initial_user_message*/ None,
                    name,
                )
                .await;
            }
            AppEvent::RawOutputModeChanged { enabled } => {
                self.apply_raw_output_mode(tui, enabled, /*notify*/ false);
            }
            AppEvent::ImplementPlanInFreshSession { plan } => {
                crate::boxed_future!(self.implement_plan_in_fresh_session(tui, app_server, plan)).await?;
            }
            AppEvent::OpenResumePicker => {
                self.pending_open_resume_picker = true;
            }
            AppEvent::ResumeSessionByIdOrName(id_or_name) => {
                match crate::lookup_session_target_with_app_server(
                    crate::resume_picker::SessionPickerAction::Resume,
                    app_server,
                    &self.config,
                    &id_or_name,
                )
                .await
                {
                    Ok(Some(target_session)) => {
                        return self
                            .resume_target_session(tui, app_server, target_session)
                            .await;
                    }
                    Ok(None) => {
                        self.chat_widget.add_error_message(format!(
                            "No saved chat found matching '{id_or_name}'."
                        ));
                    }
                    Err(err)
                        if err
                            .downcast_ref::<crate::named_session_lookup::AmbiguousSessionName>()
                            .is_some() =>
                    {
                        self.chat_widget.add_error_message(err.to_string());
                    }
                    Err(err) => return Err(err),
                }
            }
            AppEvent::ArchiveCurrentThread => {
                return self.archive_current_thread(tui, app_server).await;
            }
            AppEvent::DeleteCurrentThread => {
                return self.delete_current_thread(tui, app_server).await;
            }
            AppEvent::ForkCurrentSession { name } => {
                self.fork_current_thread(tui, app_server, name, Vec::new(), false);
                self.chat_widget.maybe_send_next_queued_input();
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ReconcilePromptRevert { id } => self.reconcile_prompt_revert(id).await,
            AppEvent::PromptRevertPrepared { id, result } => self.prompt_revert_prepared(app_server, id, result).await,
            AppEvent::PromptRevertFinished { id, result } => self.finish_prompt_revert(tui, app_server, id, result).await?,
            AppEvent::RevertSessionForPromptEdit { thread_id, selected_cell, prompt } => {
                self.start_prompt_revert(app_server, thread_id, selected_cell, prompt).await;
                tui.frame_requester().schedule_frame();
            }
            AppEvent::Rewind(input) => self.handle_rewind_input(tui, app_server, input),
            AppEvent::FinishPromptRevert {
                thread_id,
                selected_cell,
            } => self.finish_prompt_revert_projection(tui, app_server, thread_id, &selected_cell),

            AppEvent::BeginInitialHistoryReplayBuffer => {
                self.begin_initial_history_replay_buffer();
            }
            AppEvent::BeginThreadSwitchHistoryReplayBuffer => {
                self.begin_thread_switch_history_replay_buffer();
            }
            AppEvent::ParkSideCompanion { pair, thread_id, display } => {
                if self.side_threads.contains_key(&pair) {
                    self.transcript_view.sync_history_tail(&self.transcript_cells);
                    let companion = SideCompanion::new(display,
                        std::mem::take(&mut self.transcript_cells), std::mem::take(&mut self.transcript_view));
                    self.side_companions.insert(thread_id, companion);
                    self.publish_side_cells(tui, thread_id);
                }
            }
            AppEvent::SideCellPrepared { thread_id, token, cell } => {
                if let Some(companion) = self.side_companions.get_mut(&thread_id) {
                    companion.accept_prepared(token, cell);
                    self.publish_side_cells(tui, thread_id);
                }
            }
            AppEvent::ResetTranscriptForThreadSwitch => {
                self.reset_for_thread_switch(tui)?;
                self.pending_thread_switch_resets -= 1;
            }
            AppEvent::SideReplayFinished(id) => {
                self.pending_thread_switch_resets -= 1;
                let finished_current_replay = self.pending_thread_switch_resets == 0
                    && self.current_displayed_thread_id() == Some(id);
                // Keys typed against the old frame must not answer the newly revealed modal.
                self.startup_protected_input_boundary |=
                    finished_current_replay && self.chat_widget.has_active_modal();
                if finished_current_replay && let Some(companion) = self.side_companions.remove(&id)
                {
                    let replay_may_have_output = match self.thread_event_channels.get(&id) {
                        Some(channel) => channel.store.lock().await.replay_may_have_output,
                        None => true,
                    };
                    self.transcript_view = companion.view;
                    self.transcript_view.restore_after_replay(
                        &companion.cells,
                        &self.transcript_cells,
                        replay_may_have_output,
                        self.chat_widget.active_cell_transcript_key(),
                    );
                    self.scrollback_has_older_history = app_server.has_older_history(id);
                    if self.transcript_view.history == TranscriptHistoryState::LoadingOlder
                        && !app_server.has_pending_older_history_page(id)
                    {
                        self.transcript_view.history = if self.scrollback_has_older_history {
                            TranscriptHistoryState::Partial
                        } else {
                            TranscriptHistoryState::Complete
                        };
                    }
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::SidePanePressAfterReplay { peer, thread_id, area, mut press, draft_cursor } => {
                if self.pending_thread_switch_resets == 0
                    && self.current_displayed_thread_id() == Some(thread_id)
                    && self.conversation_focus_available(tui)
                {
                    let size = tui.prepare_draw_size()?;
                    if side_pane::layout(self, tui, size.into()).is_some_and(|panes| panes.passive_thread == peer) {
                        if let Some(cursor) = draft_cursor {
                            // The editor can reflow during the slide. Keep the original text position.
                            self.chat_widget.set_composer_cursor(cursor);
                        }
                        self.refresh_owned_transcript(tui)?;
                        if draft_cursor.is_some() {
                            let position = tui.terminal.last_known_cursor_pos;
                            press.column = position.x;
                            press.row = position.y;
                        } else if !self.conversation_frame.side_pane().is_some_and(|panes| panes.focused == area) {
                            return Ok(AppRunControl::Continue);
                        }
                        self.handle_owned_transcript_event(tui, app_server, &TuiEvent::Mouse(press))?;
                    }
                }
            }
            AppEvent::ResetTranscriptForThreadSwitchPreservingScreen => {
                self.reset_transcript_state_after_clear();
                tui.clear_pending_history_lines();
                self.pending_thread_switch_resets -= 1;
            }
            AppEvent::CommitRealtimeTranscriptHistory => {
                for cell in self.chat_widget.take_realtime_transcript_history() {
                    self.insert_history_cell(tui, cell);
                }
            }
            AppEvent::FollowTranscript => {
                self.transcript_view.jump_to_latest();
                tui.frame_requester().schedule_frame();
            }
            AppEvent::SubagentTextInput { thread_id, expected_turn_id, source, input } => {
                self.enqueue_subagent_turn(app_server, thread_id, expected_turn_id, input, source);
            }
            AppEvent::InsertHistoryCell(mut cell) => {
                if let Some(user) = self.chat_widget.thread_id()
                    .and_then(|thread| app_server.user_prompt_clocks(thread))
                    .and_then(|clocks| clocks.stamped(cell.as_ref()))
                {
                    cell = Box::new(user);
                }
                self.insert_history_cell(tui, cell);
            }
            AppEvent::BindUserPrompt { thread_id, identity, timestamp } => {
                if self.chat_widget.thread_id() == Some(thread_id) {
                    self.bind_user_prompt_identity(tui, identity, timestamp);
                }
            }
            AppEvent::CodeModeTranscript { thread_id, admission } => {
                self.apply_code_mode_admission(tui, thread_id, admission);
            }
            AppEvent::CodeModeProjection { thread_id, projection } => {
                if self.chat_widget.thread_id() == Some(thread_id) { self.apply_code_mode_projection(tui, projection); }
            }
            AppEvent::RecordToolOutputTokens { thread_id, id, call_id, tokens } => {
                if self.chat_widget.thread_id() == Some(thread_id) {
                    self.transcript_view.record_tool_output_tokens(id, call_id, tokens);
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::RecordToolInput { thread_id, input } => {
                if self.chat_widget.thread_id() == Some(thread_id) {
                    self.transcript_view.record_tool_input(input);
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::RecordFinalResponse { thread_id, item_id } => {
                if self.chat_widget.thread_id() == Some(thread_id) {
                    self.transcript_view.record_final_response(item_id);
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::RecordStepAdmission { thread_id, item } => {
                if self.chat_widget.thread_id() == Some(thread_id)
                    && let ThreadItemPayload::StepAdmission(admission) = item.payload.native_item()
                {
                    let response_id = admission.response_id.clone();
                    self.transcript_view.record_step_admission(item);
                    self.seat_thought(tui, &response_id);
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::TurnTipReady { thread_id, turn_id } => {
                self.turn_tips.ready(thread_id, &turn_id, self.transcript_cells.last());
                tui.frame_requester().schedule_frame();
            }
            AppEvent::EndInitialHistoryReplayBuffer => {
                self.scrollback_has_older_history = self
                    .chat_widget
                    .thread_id()
                    .is_some_and(|thread_id| app_server.has_older_history(thread_id));
                if tui.is_owned_screen() {
                    self.transcript_view.history = if self.scrollback_has_older_history { TranscriptHistoryState::Partial } else { TranscriptHistoryState::Complete };
                }
                self.finish_initial_history_replay_buffer(tui);
            }
            AppEvent::DiscardStreamItems(item_ids) => {
                self.discard_stream_items(tui, &item_ids);
            }
            AppEvent::PreparedStream(prepared) => self.handle_prepared_stream(tui, &prepared)?,
            AppEvent::ConsolidateAgentMessage {
                interrupted_item,
                source,
                item_id,
                copy_source,
                cwd,
                inline_visualization_context,
                scrollback_reflow,
                deferred_history_cell,
            } => {
                let mut cell = history_cell::AgentMarkdownCell::new_with_inline_visualizations(source, &cwd, inline_visualization_context).with_copy_source(copy_source);
                cell.interrupted_item = interrupted_item;
                cell.item_id = item_id;
                self.handle_consolidate_agent_message(
                    tui,
                    cell,
                    scrollback_reflow,
                    deferred_history_cell,
                )?;
                self.chat_widget.note_stream_consolidation_completed();
                self.insert_pending_usage_output_after_stream_shutdown(tui);
            }
            AppEvent::CorrectFlushedAgentMessage { item, cell } => {
                let belongs_to_item = |cell: &Arc<dyn HistoryCell>| {
                    cell.as_any().downcast_ref::<history_cell::AgentMarkdownCell>().is_some_and(|cell| cell.interrupted_item.as_ref() == Some(&item))
                };
                if let Some(index) = self.transcript_cells.iter().position(belongs_to_item) {
                    // Keep the reply where it first appeared, with intervening tools in order.
                    if self.transcript_cells.extract_if(index + 1.., |cell| belongs_to_item(cell)).count() > 0 {
                        self.native_history.retain(&self.transcript_cells);
                        self.transcript_view.restart_search();
                        self.transcript_view.history_loaded(&self.transcript_cells, 0..0);
                        if let Some(Overlay::Transcript(overlay)) = &mut self.overlay {
                            overlay.replace_cells(self.transcript_cells.clone());
                        }
                    }
                    self.replace_agent_message(tui, index..index + 1, cell, crate::app_event::ConsolidationScrollbackReflow::Required)?;
                } else {
                    self.insert_history_cell(tui, Box::new(cell));
                }
            }
            AppEvent::ConsolidateProposedPlan(source) => {
                let end = self.transcript_cells.len();
                let start = trailing_run_start::<history_cell::ProposedPlanStreamCell>(
                    &self.transcript_cells,
                );
                let consolidated: Arc<dyn HistoryCell> =
                    Arc::new(history_cell::new_proposed_plan(source, &self.config.cwd));

                if start < end {
                    self.native_history.consolidate(&self.transcript_cells[start..end], &consolidated);
                    if tui.is_owned_screen() {
                        self.transcript_view.replace_group(&self.transcript_cells, start..end, &consolidated);
                    } else {
                        self.transcript_view.replace_range(&self.transcript_cells, start..end, &consolidated);
                    }
                    self.transcript_cells
                        .splice(start..end, std::iter::once(consolidated.clone()));

                    if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                        t.regroup_cells(start..end, consolidated.clone());
                        tui.frame_requester().schedule_frame();
                    }

                    self.finish_required_stream_reflow(tui)?;
                } else {
                    let deferred = tui.is_owned_screen() || self.native_history.insert(&consolidated);
                    self.transcript_cells.push(consolidated.clone());
                    if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                        t.insert_cell(consolidated.clone());
                        tui.frame_requester().schedule_frame();
                    }
                    self.render_inserted_history_cell(tui, &consolidated, deferred);

                    self.maybe_finish_stream_reflow(tui)?;
                }
                self.chat_widget.note_stream_consolidation_completed();
                self.insert_pending_usage_output_after_stream_shutdown(tui);
            }
            AppEvent::StartCommitAnimation => {
                if tui.presentation_mode() != crate::tui::PresentationMode::Normal {
                    self.chat_widget.drain_unpaced_commit_tick();
                } else {
                    self.commit_animation.get_or_insert_with(|| {
                    let mut interval = tokio::time::interval_at(
                        tokio::time::Instant::now() + COMMIT_ANIMATION_TICK,
                        COMMIT_ANIMATION_TICK,
                    );
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    interval
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_animation = None;
            }
            AppEvent::Exit(mode) => {
                if matches!(mode, ExitMode::Immediate | ExitMode::ShutdownFirst)
                    && matches!(self.app_server_target, AppServerTarget::Embedded)
                    && self.has_running_owned_conversation().await
                {
                    let tx = self.app_event_tx.clone();
                    self.chat_widget.show_selection_view(SelectionViewParams {
                        title: Some("Quit and stop running sessions?".to_string()),
                        subtitle: Some("Quitting this TUI will stop its running sessions.".to_string()),
                        footer_hint: Some(standard_popup_hint_line()),
                        items: vec![
                            SelectionItem { name: "Stay in Codex".to_string(), ..Default::default() },
                            SelectionItem {
                                name: "Stop sessions and quit".to_string(),
                                actions: vec![Box::new(move |_| tx.send(AppEvent::Exit(ExitMode::ConfirmedStop)))],
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    });
                    return Ok(AppRunControl::Continue);
                }
                if matches!(mode, ExitMode::ShutdownFirst | ExitMode::ConfirmedStop | ExitMode::ShutdownAfterInterrupt) {
                    self.show_shutdown_feedback(tui)?;
                }
                return Ok(self.handle_exit_mode(app_server, mode).await);
            }
            AppEvent::RunningTaskExit { action, thread_id } => match action {
                RunningTaskExitAction::RunInBackground => {
                    return Ok(self.handle_exit_mode(app_server, ExitMode::Immediate).await);
                }
                RunningTaskExitAction::CancelTask => {
                    if self.chat_widget.thread_id() == Some(thread_id)
                        && self.chat_widget.is_agent_turn_running()
                        && self.chat_widget.submit_op(AppCommand::interrupt())
                    {
                        self.chat_widget.pause_active_goal_for_interrupt();
                    } else if self.side_threads.contains_key(&thread_id)
                        && let Err(error) = self
                            .try_submit_active_thread_op_via_app_server(
                                app_server,
                                thread_id,
                                &AppCommand::interrupt(),
                            )
                            .await
                    {
                        self.chat_widget
                            .add_error_message(format!("Failed to interrupt task: {error}"));
                    }
                }
                RunningTaskExitAction::Exit => {
                    let goal = if self.chat_widget.thread_id() == Some(thread_id)
                        && self.chat_widget.is_active_goal_turn_running() {
                        super::turn_submissions::GoalStop::Pause
                    } else {
                        super::turn_submissions::GoalStop::Preserve
                    };
                    let turn_id = self.active_turn_id_for_thread(thread_id).await;
                    self.enqueue_server_operation(app_server, thread_id,
                        super::turn_submissions::ServerOperation::Control(
                            super::turn_submissions::ControlOperation::StopAndExit { turn_id, goal }));
                }
            },
            AppEvent::Logout => self.request_logout(app_server),
            AppEvent::FatalExitRequest(message) => {
                return Ok(AppRunControl::Exit(ExitReason::Fatal(message)));
            }
            AppEvent::ImagesPrepared(id) => {
                self.chat_widget.on_images_prepared(id);
            }
            AppEvent::CodexOp(AppCommand::ListSkills { cwds, force_reload: true })
                if self.awaiting_side_replay(tui)
                    && let Some(thread_id) = self.active_thread_id
                    && self.side_pair_key(thread_id).is_some()
                    && !self.thread_unavailable(thread_id) =>
            {
                // Session replay refreshes skill metadata. Let focus paint while that RPC is pending.
                let cwd = self.chat_widget.config_ref().cwd.to_path_buf();
                if cwds.contains(&cwd) {
                    let request_id = Uuid::new_v4();
                    app_server.pending_side_skills_refresh = Some(request_id);
                    let rpc_id = app_server.next_request_id();
                    let request_handle = app_server.request_handle();
                    let app_event_tx = self.app_event_tx.clone();
                    tokio::spawn(async move {
                        let result = background_requests::fetch_skills_list(
                            request_handle,
                            rpc_id,
                            SkillsListParams { cwds, force_reload: true },
                        )
                        .await
                        .map_err(|err| format!("{err:#}"));
                        app_event_tx.send(AppEvent::SideReplaySkillsLoaded {
                            request_id, thread_id, cwd, result,
                        });
                    });
                }
            }
            AppEvent::CodexOp(op) => {
                if matches!(&op, AppCommand::UserTurn { .. }) {
                    self.chat_widget.add_error_message(
                        "The message has no captured conversation; no operation was sent.".into(),
                    );
                    return Ok(AppRunControl::Continue);
                }
                if let AppCommand::OverrideTurnContext {
                    cwd,
                    approval_policy,
                    approvals_reviewer,
                    permission_profile,
                    active_permission_profile,
                    ..
                } = &op
                    && (cwd.is_some()
                        || approval_policy.is_some()
                        || approvals_reviewer.is_some()
                        || permission_profile.is_some()
                        || active_permission_profile.is_some())
                    && self.reject_pending_permission_change()
                {
                    return Ok(AppRunControl::Continue);
                }
                let is_realtime_stop = matches!(&op, AppCommand::RealtimeConversationStop { .. });
                let realtime_stop_thread_id = match &op {
                    AppCommand::RealtimeConversationStop { thread_id, .. } => Some(*thread_id),
                    _ => None,
                };
                let realtime_speech_delivery_id = match &op {
                    AppCommand::RealtimeConversationSpeech { delivery_id, .. } => {
                        Some(*delivery_id)
                    }
                    _ => None,
                };
                let is_realtime_conversation = matches!(
                    &op,
                    AppCommand::RealtimeConversationStart { .. }
                        | AppCommand::RealtimeConversationStop { .. }
                        | AppCommand::RealtimeConversationSpeech { .. }
                );
                let parked_voice = match &op {
                    AppCommand::RealtimeConversationStart { thread_id, .. }
                    | AppCommand::RealtimeConversationStop { thread_id, .. }
                    | AppCommand::RealtimeConversationSpeech { thread_id, .. } => self
                        .background_voice
                        .as_ref()
                        .is_some_and(|owner| owner.thread_id() == Some(*thread_id)),
                    _ => false,
                };
                let visible_thread = self.active_thread_id;
                if parked_voice
                    && let Some(owner) = self.background_voice.as_mut()
                {
                    std::mem::swap(&mut self.chat_widget, owner);
                    self.active_thread_id = self.chat_widget.thread_id();
                }
                self.chat_widget.prepare_local_op_submission(&op);
                let result = self.submit_active_thread_op(app_server, op).await;
                if result.is_err()
                    && let Some(delivery_id) = realtime_speech_delivery_id
                {
                    self.chat_widget.restore_undelivered_realtime_speech(delivery_id);
                }
                if parked_voice
                    && let Some(owner) = self.background_voice.as_mut()
                {
                    std::mem::swap(&mut self.chat_widget, owner);
                    self.active_thread_id = visible_thread;
                }
                if let Err(err) = result {
                    if is_realtime_stop {
                        let chat_widget = match self.background_voice.as_deref_mut() {
                            Some(owner) if parked_voice => owner,
                            _ => &mut self.chat_widget,
                        };
                        if chat_widget.thread_id() == realtime_stop_thread_id {
                            chat_widget.record_realtime_failure(
                                crate::chatwidget::RealtimeFailureCause::AppServerRequest,
                            );
                        }
                    }
                    if self.recover_transport_error(&err) {
                        return Ok(AppRunControl::Continue);
                    }
                    let chat_widget = match self.background_voice.as_deref_mut() {
                        Some(owner) if parked_voice => owner,
                        _ => &mut self.chat_widget,
                    };
                    if is_realtime_conversation {
                        let message = format!("Voice conversation failed: {err:#}");
                        if is_realtime_stop {
                            if chat_widget.thread_id() == realtime_stop_thread_id {
                                chat_widget.reset_realtime_conversation();
                                chat_widget.add_realtime_error(message);
                            }
                        } else {
                            chat_widget.on_realtime_error(
                                message,
                                crate::chatwidget::RealtimeFailureCause::AppServerRequest,
                            );
                        }
                        tracing::error!(error = ?err, "realtime conversation request failed");
                    } else {
                        return Err(err);
                    }
                }
            }
            AppEvent::ConfirmSafetyBufferedRetry {
                thread_id,
                turn_id,
                model,
                turn,
                prompt,
            } => {
                self.chat_widget
                    .confirm_safety_buffered_retry(thread_id, turn_id, model, turn, prompt);
            }
            AppEvent::RetrySafetyBufferedTurn {
                thread_id,
                turn_id,
                model,
                turn,
                prompt,
            } => {
                self.retry_safety_buffered_turn(
                    tui,
                    app_server,
                    super::safety_buffering::SafetyBufferedRetry {
                        thread_id,
                        turn_id,
                        model,
                        turn,
                        prompt,
                    },
                )
                .await;
            }
            AppEvent::AppendMessageHistoryEntry { thread_id, text } => {
                self.append_message_history_entry(thread_id, text);
            }
            AppEvent::ContextInventory { thread_id } => {
                self.context_inventory(tui, app_server, thread_id);
            }
            AppEvent::ContextRole { thread_id, command } => {
                self.context_role(app_server, thread_id, command);
            }
            AppEvent::LiveWork(event) => {
                self.handle_live_work_event(app_server, event);
                tui.frame_requester().schedule_frame();
            }
            AppEvent::DisarmAlarm { thread_id, id } => self.disarm_alarm(app_server, thread_id, id),
            AppEvent::SetWarming { thread_id, enabled } => {
                self.set_warming(app_server, thread_id, enabled);
            }
            AppEvent::Comms { thread_id, comms } => {
                self.comms(app_server, thread_id, comms);
            }
            AppEvent::SyncThreadGitBranch {
                thread_id,
                branch,
                cwd: _cwd,
            } => {
                self.sync_thread_git_branch(app_server, thread_id, branch);
            }
            AppEvent::LookupMessageHistoryEntry {
                thread_id,
                offset,
                log_id,
            } => {
                self.lookup_message_history_entry(thread_id, offset, log_id)
                    .await?;
            }
            AppEvent::LookupMessageHistoryBatch {
                thread_id,
                cursor,
                log_id,
            } => {
                self.lookup_message_history_batch(thread_id, cursor, log_id)
                    .await?;
            }
            AppEvent::ApproveRecentAutoReviewDenial { thread_id, id } => {
                self.chat_widget
                    .approve_recent_auto_review_denial(thread_id, id);
            }
            AppEvent::SubmitThreadOp { thread_id, op } => {
                if matches!(&op, AppCommand::UserTurn { .. }) {
                    return self.dispatch_captured_user_turn(tui, app_server, thread_id, op).await;
                }
                self.submit_thread_op(app_server, thread_id, op).await?;
            }
            AppEvent::DiffResult(cwd, text) => {
                if cwds_differ(&cwd, self.chat_widget.config_ref().cwd.as_path()) {
                    return Ok(AppRunControl::Continue);
                }
                // Clear the in-progress state in the bottom pane
                self.chat_widget.on_diff_complete();
                // Enter alternate screen using TUI helper and build pager lines
                let _ = tui.enter_alt_screen();
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                self.overlay = Some(Overlay::new_static_with_lines(
                    pager_lines,
                    "D I F F".to_string(),
                    self.keymap.pager.clone(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenAppLink {
                app_id,
                title,
                description,
                instructions,
                url,
                is_installed,
                is_enabled,
            } => {
                self.chat_widget
                    .open_app_link_view(crate::bottom_pane::AppLinkViewParams {
                        app_id,
                        title,
                        description,
                        instructions,
                        url,
                        is_installed,
                        is_enabled,
                        suggest_reason: None,
                        suggestion_type: None,
                        elicitation_target: None,
                    });
            }
            AppEvent::SecuritySetupLoaded { request_id, identity, notice } => {
                tracing::debug!(current = request_id == self.chat_widget.security_setup_request_id, "handling security setup notice");
                if request_id == self.chat_widget.security_setup_request_id {
                    self.chat_widget.show_security_setup(identity, notice);
                }
            }
            AppEvent::OpenUrlInBrowser { url } => {
                self.open_url_in_browser(url);
            }
            AppEvent::OpenDesktopThread { thread_id } => {
                self.open_desktop_thread(thread_id);
            }
            AppEvent::RefreshConnectors { force_refetch } => {
                self.chat_widget.refresh_connectors(force_refetch);
            }
            AppEvent::FetchConnectorsList {
                force_refetch,
                generation,
            } => {
                if generation == self.chat_widget.connector_scope_generation() {
                    self.fetch_connectors_list(app_server, force_refetch);
                }
            }
            AppEvent::FetchInstalledConnectorMentions {
                force_refresh,
                generation,
            } => {
                if generation == self.chat_widget.connector_scope_generation() {
                    self.fetch_installed_connector_mentions(app_server, force_refresh, generation);
                }
            }
            AppEvent::PluginInstallAuthAdvance { refresh_connectors } => {
                if refresh_connectors {
                    self.chat_widget.refresh_connectors(/*force_refetch*/ true);
                }
                self.chat_widget.advance_plugin_install_auth_flow();
            }
            AppEvent::PluginInstallAuthAbandon => {
                self.chat_widget.abandon_plugin_install_auth_flow();
            }
            AppEvent::FetchPluginsList { cwd } => {
                self.fetch_plugins_list(app_server, cwd);
            }
            AppEvent::FetchHooksList { target } => {
                self.fetch_hooks_list(app_server, target);
            }
            AppEvent::OpenMarketplaceAddPrompt => {
                self.chat_widget.open_marketplace_add_prompt();
            }
            AppEvent::OpenMarketplaceAddLoading { source } => {
                self.chat_widget.open_marketplace_add_loading_popup(&source);
            }
            AppEvent::OpenMarketplaceRemoveConfirm {
                marketplace_name,
                marketplace_display_name,
            } => {
                self.chat_widget.open_marketplace_remove_confirmation(
                    marketplace_name,
                    marketplace_display_name,
                );
            }
            AppEvent::OpenMarketplaceRemoveLoading {
                marketplace_display_name,
            } => {
                self.chat_widget
                    .open_marketplace_remove_loading_popup(&marketplace_display_name);
            }
            AppEvent::OpenMarketplaceUpgradeLoading { marketplace_name } => {
                self.chat_widget
                    .open_marketplace_upgrade_loading_popup(marketplace_name.as_deref());
            }
            AppEvent::OpenPluginDetailLoading {
                plugin_display_name,
            } => {
                self.chat_widget
                    .open_plugin_detail_loading_popup(&plugin_display_name);
            }
            AppEvent::OpenPluginInstallLoading {
                plugin_display_name,
            } => {
                self.chat_widget
                    .open_plugin_install_loading_popup(&plugin_display_name);
            }
            AppEvent::OpenPluginUninstallLoading {
                plugin_display_name,
            } => {
                self.chat_widget
                    .open_plugin_uninstall_loading_popup(&plugin_display_name);
            }
            AppEvent::PluginsLoaded { cwd, result } => {
                self.chat_widget.on_plugins_loaded(cwd, result);
            }
            AppEvent::OpenPluginsList { cwd, response } => {
                self.chat_widget.open_plugins_list(cwd, response);
            }
            AppEvent::PluginRemoteSectionsLoaded {
                cwd,
                marketplaces,
                section_errors,
            } => {
                self.chat_widget.on_plugin_remote_sections_loaded(
                    cwd,
                    marketplaces,
                    section_errors,
                );
            }
            AppEvent::HooksLoaded { target, result } => {
                self.chat_widget.on_hooks_loaded(target, result);
            }
            AppEvent::FetchMarketplaceAdd { cwd, source } => {
                self.fetch_marketplace_add(app_server, cwd, source);
            }
            AppEvent::FetchMarketplaceUpgrade {
                cwd,
                marketplace_name,
            } => {
                self.fetch_marketplace_upgrade(app_server, cwd, marketplace_name);
            }
            AppEvent::MarketplaceAddLoaded {
                cwd,
                source,
                result,
            } => {
                let add_succeeded = result.is_ok();
                self.chat_widget
                    .on_marketplace_add_loaded(cwd.clone(), source, result);
                if add_succeeded && self.chat_widget.config_ref().cwd.as_path() == cwd.as_path() {
                    self.fetch_plugins_list(app_server, cwd);
                }
            }
            AppEvent::MarketplaceUpgradeLoaded { cwd, result } => {
                let marketplace_contents_changed =
                    matches!(&result, Ok(response) if !response.upgraded_roots.is_empty());
                if marketplace_contents_changed {
                    self.refresh_plugin_mentions_after_config_write();
                }
                self.chat_widget
                    .on_marketplace_upgrade_loaded(cwd.clone(), result);
                if self.chat_widget.config_ref().cwd.as_path() == cwd.as_path() {
                    self.fetch_plugins_list(app_server, cwd);
                }
            }
            AppEvent::FetchMarketplaceRemove {
                cwd,
                marketplace_name,
                marketplace_display_name,
            } => {
                self.fetch_marketplace_remove(
                    app_server,
                    cwd,
                    marketplace_name,
                    marketplace_display_name,
                );
            }
            AppEvent::MarketplaceRemoveLoaded {
                cwd,
                marketplace_name,
                marketplace_display_name,
                result,
            } => {
                let remove_succeeded = result.is_ok();
                self.chat_widget.on_marketplace_remove_loaded(
                    cwd.clone(),
                    marketplace_name,
                    marketplace_display_name,
                    result,
                );
                if remove_succeeded && self.chat_widget.config_ref().cwd.as_path() == cwd.as_path()
                {
                    self.refresh_plugin_mentions_after_config_write();
                    self.fetch_plugins_list(app_server, cwd);
                }
            }
            AppEvent::FetchPluginDetail { cwd, params } => {
                self.fetch_plugin_detail(app_server, cwd, params);
            }
            AppEvent::PluginDetailLoaded { cwd, result } => {
                self.chat_widget.on_plugin_detail_loaded(cwd, result);
            }
            AppEvent::FetchPluginInstall {
                cwd,
                location,
                plugin_name,
                plugin_display_name,
            } => {
                self.fetch_plugin_install(
                    app_server,
                    cwd,
                    location,
                    plugin_name,
                    plugin_display_name,
                );
            }
            AppEvent::FetchPluginUninstall {
                cwd,
                plugin_id,
                plugin_display_name,
            } => {
                self.fetch_plugin_uninstall(app_server, cwd, plugin_id, plugin_display_name);
            }
            AppEvent::SetPluginEnabled {
                cwd,
                plugin_id,
                enabled,
            } => {
                self.set_plugin_enabled(app_server, cwd, plugin_id, enabled);
            }
            AppEvent::PluginInstallLoaded {
                cwd,
                location,
                plugin_name,
                plugin_display_name,
                result,
            } => {
                let install_succeeded = result.is_ok();
                if install_succeeded {
                    self.refresh_plugin_mentions_after_config_write();
                }
                let should_refresh_plugin_detail = self.chat_widget.on_plugin_install_loaded(
                    cwd.clone(),
                    location.clone(),
                    plugin_name.clone(),
                    plugin_display_name,
                    result,
                );
                if install_succeeded && self.chat_widget.config_ref().cwd.as_path() == cwd.as_path()
                {
                    self.fetch_plugins_list(app_server, cwd.clone());
                    if should_refresh_plugin_detail {
                        let (marketplace_path, remote_marketplace_name) =
                            location.into_request_params();
                        self.fetch_plugin_detail(
                            app_server,
                            cwd,
                            PluginReadParams {
                                marketplace_path,
                                remote_marketplace_name,
                                plugin_name,
                            },
                        );
                    }
                }
            }
            AppEvent::PluginEnabledSet {
                cwd,
                plugin_id,
                enabled,
                result,
            } => {
                let queued_enabled = self
                    .pending_plugin_enabled_writes
                    .get_mut(&plugin_id)
                    .and_then(Option::take);
                let should_apply_result = if let Some(queued_enabled) = queued_enabled
                    && (result.is_err() || queued_enabled != enabled)
                {
                    self.spawn_plugin_enabled_write(
                        app_server,
                        cwd.clone(),
                        plugin_id.clone(),
                        queued_enabled,
                    );
                    false
                } else {
                    true
                };
                if should_apply_result {
                    self.pending_plugin_enabled_writes.remove(&plugin_id);
                    let update_succeeded = result.is_ok();
                    if update_succeeded {
                        self.refresh_plugin_mentions_after_config_write();
                    }
                    self.chat_widget
                        .on_plugin_enabled_set(cwd, plugin_id, enabled, result);
                }
            }
            AppEvent::FetchMcpInventory { detail, thread_id } => {
                self.fetch_mcp_inventory(app_server, detail, thread_id);
            }
            AppEvent::StartMcpLogin { name, thread_id } => {
                if self.pending_mcp_login_start.is_some() {
                    self.chat_widget.add_info_message(
                        "MCP sign-in is starting. Wait for it to finish before trying again.".to_string(),
                        /*hint*/ None,
                    );
                    return Ok(AppRunControl::Continue);
                }
                let request_id = format!("mcp-login-{}", uuid::Uuid::new_v4());
                self.pending_mcp_login_start = Some(PendingMcpLoginStart {
                    request_id: request_id.clone(),
                    name: name.clone(),
                    thread_id,
                    completions: Vec::new(),
                });
                self.start_mcp_login(app_server, request_id, name, thread_id);
            }
            AppEvent::McpLoginStarted { request_id, result } => {
                if let Some(pending) = self
                    .pending_mcp_login_start
                    .take_if(|pending| pending.request_id == request_id)
                {
                    match result {
                        Ok(response) => {
                            // Track the latest attempt per server: unrelated OAuth logins can
                            // remain open while a user retries this server.
                            if let Some(login_id) = response.login_id.as_ref() {
                                self.active_mcp_login_ids
                                    .insert(pending.name.clone(), login_id.clone());
                            }
                            let completed = pending.completions.iter().any(|completion| {
                                completion.login_id == response.login_id
                                    && completion.name == pending.name
                            });
                            if !completed {
                                self.open_url_in_browser(response.authorization_url);
                            }
                        }
                        Err(error) => {
                            self.enqueue_thread_notification(
                                pending.thread_id,
                                ServerNotification::McpServerOauthLoginCompleted(
                                    codex_app_server_protocol::McpServerOauthLoginCompletedNotification {
                                        name: pending.name,
                                        thread_id: Some(pending.thread_id.to_string()),
                                        login_id: None,
                                        success: false,
                                        error: Some(error),
                                    },
                                ),
                            ).await?;
                        }
                    }
                    // A rejected start leaves the old attempt current. A successful replacement
                    // has changed its ID, so the old cancellation is discarded here.
                    for completion in pending.completions {
                        if completion.login_id.is_some() {
                            if completion.login_id.as_ref()
                                != self.active_mcp_login_ids.get(&completion.name)
                            {
                                continue;
                            }
                            self.active_mcp_login_ids.remove(&completion.name);
                        }
                        if let Some(thread_id) = completion.thread_id.as_deref()
                            .and_then(|id| ThreadId::from_string(id).ok())
                        {
                            self.enqueue_thread_notification(
                                thread_id,
                                ServerNotification::McpServerOauthLoginCompleted(completion),
                            ).await?;
                        }
                    }
                }
            }
            AppEvent::McpInventoryLoaded {
                result,
                detail,
                thread_id,
            } => {
                self.handle_mcp_inventory_result(result, detail, thread_id);
            }
            AppEvent::RefreshSkills => self.refresh_skills(app_server),
            AppEvent::SideReplaySkillsLoaded { request_id, thread_id, cwd, result } => {
                if app_server.pending_side_skills_refresh == Some(request_id) {
                    app_server.pending_side_skills_refresh = None;
                    if !self.awaiting_side_replay(tui)
                        && self.current_displayed_thread_id() == Some(thread_id)
                        && !cwds_differ(&cwd, self.chat_widget.config_ref().cwd.as_path())
                    {
                        self.handle_skills_list_result(
                            result.map_err(|err| color_eyre::eyre::eyre!(err)),
                            "failed to refresh skills",
                        );
                    }
                }
            }
            AppEvent::SkillsListLoaded { result, .. } => {
                self.handle_skills_list_result(
                    result.map_err(|err| color_eyre::eyre::eyre!(err)),
                    "failed to refresh skills",
                );
                self.skill_load_warnings.startup_complete = true;
            }
            AppEvent::StartFileSearch(query) => {
                self.file_search.on_user_query(query.clone());
                if let Some(thread_id) = self.active_thread_id
                    && app_server.task_tools_available(thread_id)
                    && self.config.features.enabled(Feature::MentionsV2)
                {
                    let cwd = self
                        .thread_cwd(thread_id)
                        .await
                        .map(|cwd| cwd.to_path_buf())
                        .or_else(|| {
                            app_server
                                .remote_cwd_override()
                                .map(std::path::Path::to_path_buf)
                        })
                        .unwrap_or_else(|| self.config.cwd.to_path_buf());
                    crate::task_mentions::spawn_search(
                        app_server.request_handle(),
                        query,
                        thread_id,
                        cwd,
                        app_server.task_search_generation(),
                        self.app_event_tx.clone(),
                    );
                }
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_widget.apply_file_search_result(query, matches);
            }
            AppEvent::TaskSearchResult {
                thread_id,
                query,
                matches,
            } => {
                if self.active_thread_id == Some(thread_id)
                    && app_server.task_tools_available(thread_id)
                {
                    self.chat_widget.on_task_search_result(&query, matches);
                }
            }
            AppEvent::RefreshRateLimits { origin } => {
                self.refresh_rate_limits(app_server, origin);
            }
            AppEvent::RefreshClaudeQuota { request_id, account } => {
                self.refresh_claude_quota(app_server, request_id, account);
            }
            AppEvent::ClaudeQuotaLoaded { request_id, result } => {
                self.chat_widget.finish_status_claude_quota_refresh(request_id, &result);
            }
            AppEvent::OpenAccountPicker { thread_id } => self.open_account_picker(app_server, thread_id),
            AppEvent::SelectAccount { thread_id, account_id, provider } => self.select_account(app_server, thread_id, account_id, provider),
            AppEvent::RenameAccountPrompt { thread_id, provider, account_id, nickname } => {
                if self.current_displayed_thread_id() == Some(thread_id) {
                    self.chat_widget.show_account_nickname_prompt(thread_id, provider, account_id, nickname);
                }
            }
            AppEvent::SetAccountNickname { thread_id, provider, account_id, nickname } => self.set_account_nickname(app_server, thread_id, provider, account_id, nickname),
            AppEvent::AccountNicknameSetFinished { thread_id, result } => self.finish_account_nickname_set(app_server, thread_id, result),
            AppEvent::AddClaudeAccount => self.add_claude_account(app_server),
            AppEvent::ClaudeLoginStarted { start_id, result } => self.finish_claude_login_start(start_id, result),
            AppEvent::CancelClaudeLogin { login_id } => self.cancel_claude_login(app_server, login_id),
            AppEvent::ClaudeLoginCancelFailed { error } => self.chat_widget.add_error_message(error),
            AppEvent::AccountPickerLoaded { thread_id, serial, result } => self.apply_account_picker(thread_id, serial, result),
            AppEvent::AccountSelectionFinished { thread_id, result } => self.finish_account_selection(app_server, thread_id, result),
            AppEvent::Usage { args } => self.read_displayed_account(app_server, Some(args)),
            AppEvent::RefreshConversationAccount { thread_id } => self.refresh_conversation_account(app_server, thread_id),
            AppEvent::ConversationAccountLoaded { thread_id, serial, result } => self.apply_conversation_account(app_server, thread_id, serial, result),
            AppEvent::ApplyBackendBannerFallback { thread_id } => {
                if self.active_thread_id == Some(thread_id)
                    && self.chat_widget.thread_id() == Some(thread_id)
                {
                    self.apply_backend_banner_fallback(app_server).await;
                    if !self.rate_limit_refresh_state.has_pending_recovery() {
                        self.chat_widget.finish_rate_limit_recovery();
                    }
                    self.refresh_rate_limits(app_server, RateLimitRefreshOrigin::Periodic);
                }
            }
            AppEvent::RestoreBackgroundTerminals(thread_id) => {
                if self.chat_widget.thread_id() == Some(thread_id) {
                    let request_id = self.chat_widget.begin_background_terminal_restore();
                    let mut server = app_server.request_view();
                    let tx = self.app_event_tx.clone();
                    tokio::spawn(async move {
                        let terminals = server.list_background_terminals(thread_id).await.unwrap_or_default();
                        tx.send(AppEvent::BackgroundTerminalsLoaded { request_id, terminals });
                    });
                }
            }
            AppEvent::BackgroundTerminalsLoaded { request_id, terminals } => {
                self.chat_widget.restore_background_terminals(request_id, terminals);
            }
            AppEvent::RefreshThreadUsage {
                thread_id,
                request_id,
            } => {
                self.refresh_thread_usage(app_server, thread_id, request_id);
            }
            AppEvent::RefreshStatusLineWorkspaceHeadline { request_id } => {
                self.refresh_status_line_workspace_headline(app_server, request_id);
            }
            AppEvent::OpenThreadGoalMenu { thread_id } => {
                self.open_thread_goal_menu(app_server, thread_id).await;
            }
            AppEvent::OpenThreadGoalEditor { thread_id } => {
                self.open_thread_goal_editor(app_server, thread_id).await;
            }
            AppEvent::SetThreadGoalDraft {
                thread_id,
                draft,
                mode,
            } => {
                self.set_thread_goal_draft(app_server, thread_id, draft, mode)
                    .await;
            }
            AppEvent::SetThreadGoalStatus { thread_id, status } => {
                self.set_thread_goal_status(app_server, thread_id, status)
                    .await;
            }
            AppEvent::ClearThreadGoal { thread_id } => {
                self.clear_thread_goal(app_server, thread_id).await;
            }
            AppEvent::ThreadGoalBoxes { thread_id, command } => {
                self.thread_goal_boxes(app_server, thread_id, command);
            }
            AppEvent::SendAddCreditsNudgeEmail { credit_type } => {
                if let Some(request_id) = self
                    .chat_widget
                    .start_add_credits_nudge_email_request(credit_type)
                {
                    self.send_add_credits_nudge_email(app_server, request_id, credit_type);
                }
            }
            AppEvent::AddCreditsNudgeEmailFinished { request_id, result } => {
                self.chat_widget
                    .finish_add_credits_nudge_email_request(request_id, result);
            }
            AppEvent::RateLimitsLoaded {
                request_id,
                origin,
                hard_stop_generation,
                result,
            } => {
                let accepted = match self.rate_limit_refresh_state.finish(
                    request_id,
                    hard_stop_generation,
                    self.rate_limit_hard_stop_generation,
                    if result.is_ok() {
                        RateLimitReadStatus::Succeeded
                    } else {
                        RateLimitReadStatus::Failed
                    },
                ) {
                    RateLimitRefreshOutcome::Apply => true,
                    RateLimitRefreshOutcome::Ignore => false,
                    RateLimitRefreshOutcome::RefreshRecovery => {
                        // Start in this account's event turn; a queued refresh could cross an account change.
                        self.refresh_rate_limits(app_server, RateLimitRefreshOrigin::Recovery);
                        false
                    }
                };
                match result {
                Ok(mut response) => {
                    let reset_credit_confirmation = response.reset_credit_confirmation.take();
                    let rate_limit_reset_credits = response.rate_limit_reset_credits.clone();
                    let snapshots = if accepted
                    {
                        self.chat_widget.apply_usage_notice_read(request_id);
                        self.chat_widget.update_backend_banner(&response);
                        self.apply_backend_banner_fallback(app_server).await;
                        app_server_rate_limit_snapshots(response)
                    } else {
                        Vec::new()
                    };
                    match origin {
                        RateLimitRefreshOrigin::Recovery | RateLimitRefreshOrigin::Periodic => {
                            for snapshot in snapshots {
                                self.chat_widget.on_rate_limit_snapshot(Some(snapshot));
                            }
                        }
                        RateLimitRefreshOrigin::StartupPrefetch {
                            reset_hint_request_id,
                        } => {
                            if self.chat_widget.finish_rate_limit_reset_hint_refresh(
                                reset_hint_request_id,
                                snapshots,
                                rate_limit_reset_credits.ok_or_else(|| {
                                    "account/rateLimits/read response did not include rateLimitResetCredits"
                                        .to_string()
                                }),
                            ) {
                                self.insert_pending_usage_output_if_ready(tui);
                            }
                            tui.frame_requester().schedule_frame();
                        }
                        RateLimitRefreshOrigin::ResetConsume { request_id } => {
                            self.chat_widget.finish_post_consume_reset_credits_refresh(
                                request_id,
                                snapshots,
                                rate_limit_reset_credits.ok_or_else(|| {
                                    "account/rateLimits/read response did not include rateLimitResetCredits"
                                        .to_string()
                                }),
                            );
                            tui.frame_requester().schedule_frame();
                        }
                        RateLimitRefreshOrigin::StatusCommand { request_id } => {
                            self.chat_widget
                                .finish_status_rate_limit_refresh(request_id, snapshots);
                        }
                        RateLimitRefreshOrigin::UsageMenu { request_id } => {
                            self.chat_widget.finish_usage_menu_rate_limit_refresh(
                                request_id,
                                snapshots,
                                rate_limit_reset_credits.ok_or_else(|| {
                                    "account/rateLimits/read response did not include rateLimitResetCredits"
                                    .to_string()
                                }),
                            );
                        }
                        RateLimitRefreshOrigin::ResetPicker { request_id } => {
                            self.chat_widget.finish_rate_limit_reset_credits_refresh(
                                request_id,
                                snapshots,
                                rate_limit_reset_credits.ok_or_else(|| {
                                    "account/rateLimits/read response did not include rateLimitResetCredits"
                                        .to_string()
                                }),
                                reset_credit_confirmation,
                            );
                        }
                    }
                }
                Err(err) => {
                    // A failed read is not authoritative recovery. Keep the last valid banner.
                    tracing::warn!("account/rateLimits/read failed during TUI refresh: {err}");
                    match origin {
                        RateLimitRefreshOrigin::Recovery | RateLimitRefreshOrigin::Periodic => {
                            // Re-evaluate snapshot age even when the backend cannot refresh it.
                            // This updates display freshness without authorizing model recovery.
                            self.chat_widget.refresh_status_surfaces();
                        },
                        RateLimitRefreshOrigin::StartupPrefetch {
                            reset_hint_request_id,
                        } => {
                            self.chat_widget.finish_rate_limit_reset_hint_refresh(
                                reset_hint_request_id,
                                Vec::new(),
                                Err(err),
                            );
                        }
                        RateLimitRefreshOrigin::ResetConsume { request_id } => {
                            self.chat_widget.finish_post_consume_reset_credits_refresh(
                                request_id,
                                Vec::new(),
                                Err(err),
                            );
                        }
                        RateLimitRefreshOrigin::StatusCommand { request_id } => {
                            self.chat_widget
                                .finish_status_rate_limit_refresh(request_id, Vec::new());
                        }
                        RateLimitRefreshOrigin::UsageMenu { request_id } => {
                            self.chat_widget.finish_usage_menu_rate_limit_refresh(
                                request_id,
                                Vec::new(),
                                Err(err),
                            );
                        }
                        RateLimitRefreshOrigin::ResetPicker { request_id } => {
                            self.chat_widget.finish_rate_limit_reset_credits_refresh(
                                request_id,
                                Vec::new(),
                                Err(err),
                                None,
                            );
                        }
                    }
                }
                }
                if (accepted || matches!(
                    origin,
                    RateLimitRefreshOrigin::Recovery | RateLimitRefreshOrigin::ResetConsume { .. }
                )) && !self.rate_limit_refresh_state.has_pending_recovery()
                {
                    self.chat_widget.finish_rate_limit_recovery();
                }
            },
            AppEvent::OpenAnalytics { view: summary_view } => {
                tui.enter_alt_screen()?;
                let mut view = self.retained_analytics.take().unwrap_or_else(|| {
                    Box::new(crate::analytics::AnalyticsView::new(self.keymap.list.clone()))
                });
                view.keymap = self.keymap.list.clone();
                if summary_view.is_some() {
                    view.select_summary(summary_view);
                }
                view.open(
                    app_server.request_handle(),
                    tui.frame_requester(),
                    self.model_catalog.try_list_models()?,
                    std::sync::Arc::new(self.config.clone()),
                );
                self.overlay = Some(Overlay::Analytics(view));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenRateLimitResetCredits => {
                let request_id = self.chat_widget.show_rate_limit_reset_loading_popup();
                self.refresh_rate_limits(
                    app_server,
                    RateLimitRefreshOrigin::ResetPicker { request_id },
                );
            }
            AppEvent::OpenRateLimitResetConfirmation {
                picker_request_id,
                confirmation,
                confirmation_gate,
                credit_id,
                reset_title,
                reset_detail,
                reset_description,
            } => {
                self.chat_widget.show_rate_limit_reset_confirmation(
                    picker_request_id,
                    confirmation,
                    confirmation_gate,
                    credit_id,
                    reset_title,
                    reset_detail,
                    reset_description,
                );
            }
            AppEvent::ConsumeRateLimitResetCredit {
                confirmation,
                idempotency_key,
                credit_id,
            } => {
                if let Some(request_id) = self
                    .chat_widget
                    .start_rate_limit_reset_consumption(&idempotency_key)
                {
                    self.consume_rate_limit_reset_credit(
                        app_server,
                        request_id,
                        confirmation,
                        idempotency_key,
                        credit_id,
                    );
                }
            }
            AppEvent::RateLimitResetCreditConsumed {
                request_id,
                confirmation,
                idempotency_key,
                credit_id,
                result,
            } => {
                if let Err(err) = &result {
                    tracing::warn!(
                        "account/rateLimitResetCredit/consume failed during TUI request: {err}"
                    );
                }
                if self.chat_widget.finish_rate_limit_reset_consume(
                    request_id,
                    confirmation,
                    idempotency_key,
                    credit_id,
                    result,
                ) {
                    // Reads started before redemption must not restore the pre-reset banner.
                    self.rate_limit_hard_stop_generation =
                        self.rate_limit_hard_stop_generation.wrapping_add(1);
                    self.rate_limit_refresh_state.invalidate_recovery();
                    self.chat_widget.clear_backend_banner();
                    self.refresh_rate_limits(
                        app_server,
                        RateLimitRefreshOrigin::ResetConsume { request_id },
                    );
                }
            }
            AppEvent::ThreadUsageLoaded {
                thread_id,
                request_id,
                result,
            } => {
                self.finish_thread_usage_refresh(tui, thread_id, request_id, result)?;
            }
            AppEvent::AgentsOverviewUsageLoaded { thread_id, request_id, result } => {
                self.finish_agents_overview_usage(thread_id, request_id, result);
            }
            AppEvent::CommitPendingUsageOutput => {
                self.insert_pending_usage_output_if_ready(tui);
            }
            AppEvent::CommitPendingUsageOutputAfterStreamShutdown => {
                self.insert_pending_usage_output_after_stream_shutdown(tui);
            }
            AppEvent::ConnectorsLoaded {
                thread_id,
                cwd,
                generation,
                result,
                is_final,
            } => {
                if thread_id == self.current_displayed_thread_id()
                    && cwd.as_path() == self.chat_widget.config_ref().cwd.as_path()
                    && generation == self.chat_widget.connector_scope_generation()
                {
                    self.chat_widget.on_connectors_loaded(result, is_final);
                }
            }
            AppEvent::InstalledConnectorMentionsLoaded {
                thread_id,
                cwd,
                generation,
                result,
            } => {
                if thread_id == self.current_displayed_thread_id()
                    && cwd.as_path() == self.chat_widget.config_ref().cwd.as_path()
                    && generation == self.chat_widget.connector_scope_generation()
                {
                    self.chat_widget
                        .on_connector_mentions_loaded(generation, result);
                }
            }
            AppEvent::UpdateReasoningEffort(effort) => {
                self.model_selection_generation.advance();
                self.on_update_reasoning_effort(effort.clone());
                self.sync_active_thread_reasoning_setting(app_server, effort)
                    .await;
            }
            AppEvent::UpdateLunaReserveReasoning { thread_id, effort } => {
                self.update_luna_reserve_reasoning(app_server, thread_id, effort)
                    .await;
            }
            AppEvent::UpdateModel(model) => {
                if self
                    .active_thread_model_setting_update_params(model.clone())
                    .is_some_and(|params| params.permissions.is_some())
                    && self.reject_pending_permission_change()
                {
                    return Ok(AppRunControl::Continue);
                }
                self.model_selection_generation.advance();
                let model_changed = self.model_selection_changes_current_model(&model);
                if model_changed {
                    self.chat_widget.set_model(&model);
                    self.sync_active_thread_model_setting(app_server, model, /*effort*/ None)
                        .await;
                    self.sync_active_thread_service_tier_to_cached_session()
                        .await;
                }
            }
            AppEvent::ApplyModelSelection { model, effort } => {
                self.apply_default_model_selection(app_server, model, effort)
                    .await;
            }
            AppEvent::ScopedModelPicker { .. } | AppEvent::AstraSelectedFromModelPicker { .. } => unreachable!("picker event unwrapped"),
            AppEvent::BackgroundVoiceError { thread_id, message } => {
                if self.chat_widget.thread_id() == Some(thread_id) {
                    self.chat_widget.add_error_message(message);
                } else {
                    self.background_voice_error = Some((thread_id, message));
                }
            }
            AppEvent::RealtimeConversationStateChanged => {
                self.repaint_agents_overview();
            }
            AppEvent::VoiceControl { thread_id, control } => {
                if thread_id == self.chat_widget.thread_id() || self.voice_owner_thread_id().is_some() {
                    self.control_voice(control);
                }
            }
            AppEvent::RealtimeSessionLockStopped { thread_id } => {
                if let Some(owner) = self.voice_widget_for_thread(thread_id) {
                    owner.stop_if_session_locked();
                }
            }
            AppEvent::RealtimeWebrtcOfferCreated {
                thread_id,
                attempt_id,
                result,
            } => {
                if let Some(owner) = self.voice_widget_for_thread(thread_id) {
                    owner.on_realtime_webrtc_offer_created(thread_id, attempt_id, result);
                } else if let Ok(offer) = result {
                    offer.handle.close();
                }
            }
            AppEvent::RealtimeWebrtcConnected {
                thread_id,
                attempt_id,
                result,
            } => {
                if let Some(owner) = self.voice_widget_for_thread(thread_id) {
                    owner.on_realtime_webrtc_connected(attempt_id, result);
                }
            }
            AppEvent::StopRealtimeConversation { thread_id } => {
                match tokio::time::timeout(
                    SHUTDOWN_FIRST_EXIT_TIMEOUT,
                    app_server.thread_realtime_stop(thread_id),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(
                        %thread_id,
                        %error,
                        "failed to stop voice conversation after switching threads"
                    ),
                    Err(_) => tracing::warn!(
                        %thread_id,
                        "timed out stopping voice conversation after switching threads"
                    ),
                }
            }
            AppEvent::SettingsSelectionClosed => {
                #[cfg(any(target_os = "windows", test))]
                let windows_sandbox_setup_pending =
                    self.windows_sandbox.pending_setup.is_some();
                #[cfg(not(any(target_os = "windows", test)))]
                let windows_sandbox_setup_pending = false;
                if self.chat_widget.no_modal_or_popup_active()
                    && !windows_sandbox_setup_pending
                {
                    self.chat_widget
                        .recover_inline_permission_profile_selection();
                }
                self.app_event_tx.send(AppEvent::SettingsSelectionSettled);
            }
            AppEvent::SettingsSelectionSettled => {
                if !self.chat_widget.permission_popup_pending()
                    && self.chat_widget.no_modal_or_popup_active()
                    && !self
                        .chat_widget
                        .inline_permission_profile_selection_pending()
                    && !self
                        .chat_widget
                        .thread_id()
                        .is_some_and(|thread_id| {
                            self.pending_server_profiles.contains_key(&thread_id)
                                || self
                                    .agents_overview
                                    .requested_permission_profiles
                                    .get(&thread_id)
                                    .is_some_and(|selection| {
                                        let config = self.chat_widget.config_ref();
                                        config
                                            .permissions
                                            .active_permission_profile()
                                            .is_none_or(|active| {
                                                active.id != selection.profile_id
                                            })
                                            || selection.approval_policy.is_some_and(|policy| {
                                                config.permissions.approval_policy.value()
                                                    != policy.to_core()
                                            })
                                            || selection
                                                .approvals_reviewer
                                                .is_some_and(|reviewer| {
                                                    config.approvals_reviewer != reviewer
                                                })
                                    })
                        })
                {
                    let config = self.chat_widget.config_ref();
                    let permissions_override = Self::turn_permissions_override_from_config(
                        config,
                        config.permissions.active_permission_profile().as_ref(),
                        self.runtime_permission_profile_override
                            .as_ref()
                            .and_then(RuntimePermissionProfileOverride::turn_permission_profile),
                    );
                    if turn_permissions_overrides(permissions_override, config.cwd.as_path())
                        .is_ok()
                    {
                        self.chat_widget
                            .set_queue_autosend_suppressed(/*suppressed*/ false);
                        self.chat_widget.maybe_send_next_queued_input();
                    }
                }
            }
            AppEvent::FetchPermissionProfiles { request_id, thread_cwd } => {
                if self.chat_widget.permission_popup_request_is_current(request_id) {
                    crate::permission_discovery::fetch(
                        app_server,
                        request_id,
                        self.chat_widget.config_ref(),
                        thread_cwd.as_deref(),
                        self.app_event_tx.clone(),
                    );
                }
            }
            AppEvent::PermissionProfilesLoaded { request_id, result } => {
                self.chat_widget.on_permission_profiles_loaded(request_id, result);
            }
            AppEvent::CancelModelRequest { request_id } => {
                self.chat_widget.cancel_model_request(request_id);
            }
            AppEvent::FetchModels { request_id } => {
                if self.chat_widget.model_popup_request_is_current(request_id) {
                    app_server.fetch_models(
                        request_id,
                        self.chat_widget.config_ref().model_provider_id.clone(),
                        self.current_displayed_thread_id(),
                        self.app_event_tx.clone(),
                    );
                }
            }
            AppEvent::ModelsLoaded { request_id, result: Ok(models) }
                if self.chat_widget.model_popup_request_is_current(request_id)
                    && self.chat_widget.status_account_display() == Some(&StatusAccountDisplay::ApiKey) =>
            {
                let checked = app_server.constrain_api_key_models(
                    self.chat_widget.config_ref(),
                    self.chat_widget.thread_id(),
                    /*requirements*/ None,
                    models.models,
                );
                let providers = models.providers;
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    tx.send(AppEvent::ApiKeyCompatModelsLoaded { request_id, result: Ok(crate::model_catalog::ModelListing { models: checked.await, providers }) });
                });
            }
            AppEvent::ModelsLoaded { request_id, result }
            | AppEvent::ApiKeyCompatModelsLoaded { request_id, result } => {
                self.chat_widget.on_models_loaded(request_id, result);
                let catalog = self.chat_widget.model_catalog();
                if !Arc::ptr_eq(&self.model_catalog, &catalog) {
                    self.model_catalog = catalog;
                    app_server.set_available_models(self.model_catalog.try_list_models()?);
                    self.sync_active_thread_service_tier_to_cached_session().await;
                }
            }
            AppEvent::ProviderModel(event) => self.handle_provider_model_event(tui, app_server, event).await,
            AppEvent::OpenRouter(event) => self.handle_openrouter_event(app_server, event),
            AppEvent::OpenProviderReasoning { provider, model } => self.chat_widget.show_provider_reasoning(&provider, &model),
            AppEvent::OpenReasoningPopup { model } => {
                self.chat_widget.open_reasoning_popup(model);
            }
            AppEvent::OpenAdvancedReasoningPopup { model } => {
                self.chat_widget.open_advanced_reasoning_popup(model);
            }
            AppEvent::ApplyAdvancedReasoning { model, effort } => {
                self.app_event_tx.send(AppEvent::FollowTranscript);
                if self
                    .active_thread_model_setting_update_params(model.clone())
                    .is_some_and(|params| params.permissions.is_some())
                    && self.reject_pending_permission_change()
                {
                    return Ok(AppRunControl::Continue);
                }
                self.model_selection_generation.advance();
                let model_changed = self.model_selection_changes_current_model(&model);
                let default_effort =
                    self.on_apply_advanced_reasoning(model.as_str(), effort.clone());
                if model_changed {
                    self.sync_active_thread_model_setting(
                        app_server,
                        model.clone(),
                        Some(effort.clone()),
                    )
                    .await;
                } else if let Some(mut params) =
                    self.active_thread_reasoning_setting_update_params(Some(effort.clone()))
                {
                    params.collaboration_mode =
                        Some(self.chat_widget.effective_collaboration_mode());
                    self.send_thread_settings_update(app_server, params).await;
                }
                self.sync_active_thread_service_tier_to_cached_session()
                    .await;

                self.persist_conversation_model_selection(app_server, &model, &effort, default_effort.as_ref());
            }
            AppEvent::OpenPlanReasoningScopePrompt { model, effort } => {
                self.chat_widget.open_plan_reasoning_scope_prompt(model, effort);
            }
            AppEvent::OpenAllModelsPopup => {
                self.chat_widget.open_all_models_popup();
            }
            AppEvent::OpenFullAccessConfirmation {
                preset,
                return_to_permissions,
                profile_selection,
            } => {
                self.chat_widget.open_full_access_confirmation(
                    preset,
                    return_to_permissions,
                    profile_selection,
                );
            }
            AppEvent::ApplyPermissionShortcut { thread_id, selection } => {
                self.apply_permission_shortcut(app_server, thread_id, selection).await;
            }
            AppEvent::LaunchExternalEditor => {
                if self.chat_widget.external_editor_state() == ExternalEditorState::Active {
                    self.launch_external_editor(tui).await;
                }
            }
            AppEvent::RefreshWindowsSandbox { thread_id } => {
                #[cfg(any(target_os = "windows", test))]
                self.refresh_windows_sandbox_for_thread(app_server, thread_id).await;
                #[cfg(not(any(target_os = "windows", test)))]
                let _ = thread_id;
            }
            AppEvent::OpenWindowsSandboxEnablePrompt {
                preset,
                profile_selection,
            } => {
                self.chat_widget
                    .open_windows_sandbox_enable_prompt(preset, profile_selection);
            }
            AppEvent::OpenWindowsSandboxFallbackPrompt {
                preset,
                profile_selection,
            } => {
                #[cfg(any(target_os = "windows", test))]
                {
                    self.windows_sandbox.pending_setup = None;
                }
                self.session_telemetry.counter(
                    "codex.windows_sandbox.fallback_prompt_shown",
                    /*inc*/ 1,
                    &[],
                );
                self.chat_widget.clear_windows_sandbox_setup_status();
                if let Some(started_at) = self.windows_sandbox.setup_started_at.take() {
                    self.session_telemetry.record_duration(
                        "codex.windows_sandbox.elevated_setup_duration_ms",
                        started_at.elapsed(),
                        &[("result", "failure")],
                    );
                }
                self.chat_widget
                    .open_windows_sandbox_fallback_prompt(preset, profile_selection);
            }
            AppEvent::BeginWindowsSandboxElevatedSetup {
                preset,
                profile_selection,
            } => {
                #[cfg(target_os = "windows")]
                self.begin_windows_sandbox_setup(
                    app_server,
                    preset,
                    profile_selection,
                    WindowsSandboxEnableMode::Elevated,
                )
                .await;
                #[cfg(not(target_os = "windows"))]
                let _ = (preset, profile_selection);
            }
            AppEvent::BeginWindowsSandboxLegacySetup {
                preset,
                profile_selection,
            } => {
                #[cfg(target_os = "windows")]
                self.begin_windows_sandbox_setup(
                    app_server,
                    preset,
                    profile_selection,
                    WindowsSandboxEnableMode::Legacy,
                )
                .await;
                #[cfg(not(target_os = "windows"))]
                let _ = (preset, profile_selection);
            }
            AppEvent::EnableWindowsSandboxForAgentMode {
                preset,
                mode,
                profile_selection,
            } => {
                #[cfg(any(target_os = "windows", test))]
                {
                    self.windows_sandbox.pending_setup = None;
                }
                #[cfg(target_os = "windows")]
                {
                    self.chat_widget.clear_windows_sandbox_setup_status();
                    if let Some(started_at) = self.windows_sandbox.setup_started_at.take()
                        && mode == WindowsSandboxEnableMode::Elevated
                    {
                        self.session_telemetry.record_duration(
                            "codex.windows_sandbox.elevated_setup_duration_ms",
                            started_at.elapsed(),
                            &[("result", "success")],
                        );
                    }
                    let selected_mode = match mode {
                        WindowsSandboxEnableMode::Elevated => WindowsSandboxSetupMode::Elevated,
                        WindowsSandboxEnableMode::Legacy => WindowsSandboxSetupMode::Unelevated,
                    };
                    let elevated_enabled = selected_mode == WindowsSandboxSetupMode::Elevated;
                    if self
                        .verify_windows_sandbox_mode_after_setup(app_server, selected_mode)
                        .await
                    {
                            self.chat_widget.windows_sandbox_elevated_setup_complete =
                                elevated_enabled;
                            if let Some(selection) = profile_selection {
                                let inline = self.chat_widget.complete_inline_permission_profile_selection();
                                self.select_permission_profile(app_server, selection).await;
                                if inline {
                                    self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
                                }
                            } else {
                                self.app_event_tx.send(AppEvent::CodexOp(
                                    AppCommand::override_turn_context(
                                        /*cwd*/ None,
                                        Some(AskForApproval::from(preset.approval)),
                                        Some(self.config.approvals_reviewer),
                                        Some(preset.permission_profile.clone()),
                                        Some(preset.active_permission_profile.clone()),
                                        /*model*/ None,
                                        /*effort*/ None,
                                        /*summary*/ None,
                                        /*service_tier*/ None,
                                        /*collaboration_mode*/ None,
                                    ),
                                ));
                                self.app_event_tx.send(AppEvent::UpdateAskForApprovalPolicy(
                                    AskForApproval::from(preset.approval),
                                ));
                                self.app_event_tx
                                    .send(AppEvent::UpdateActivePermissionProfile(
                                        preset.active_permission_profile.clone(),
                                    ));
                            }
                                self.chat_widget.add_plain_history_lines(vec![
                                    Line::from(vec!["• ".dim(), "Sandbox ready".into()]),
                                    Line::from(vec![
                                        "  ".into(),
                                        "Codex can now safely edit files and execute commands in your computer"
                                            .dark_gray(),
                                    ]),
                                ]);
                    } else if self
                        .chat_widget
                        .recover_inline_permission_profile_selection()
                    {
                        self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
                    } else {
                        self.chat_widget
                            .retain_input_after_failed_permission_selection();
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = (preset, mode, profile_selection);
                }
            }
            AppEvent::PersistModelSelection { model, effort } => self.persist_selected_model(app_server, model, effort),

            AppEvent::PersistDaybreakSelection { thread_id, enabled } => {
                Box::pin(self.persist_daybreak_selection(app_server, thread_id, enabled)).await;
            }
            AppEvent::DisableUnavailableDaybreak { thread_id, model } => {
                if self.chat_widget.current_model() == model
                    && self.chat_widget.should_disable_daybreak(&model)
                    && self.active_thread_id == Some(thread_id)
                {
                    self.persist_daybreak_selection_continuation(app_server, thread_id, false, move |app| {
                        if app.active_thread_id == Some(thread_id) && app.chat_widget.current_model() == model {
                            app.chat_widget.add_info_message(
                                format!("Daybreak has been turned off for new turns because it is not available for {model} with your current account and server. Prior cybersecurity context in this thread may cause a refusal. You can switch back to a model where you have Daybreak access and use /daybreak to re-enable it."),
                                /*hint*/ None,
                            );
                        }
                    });
                }
            }
            AppEvent::SelectSessionModel { model, effort, service_tier } => {
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.select_session_model(app_server, model, effort, service_tier, sparkle_model.take()).await;
            }
            AppEvent::OpenQuickDialEditor { target, dial } => {
                self.chat_widget.open_quick_dial_editor(target, dial);
            }
            AppEvent::OpenQuickDialSavePicker(dial) => {
                self.chat_widget.open_quick_dial_save_picker(dial);
            }
            AppEvent::ConfirmQuickDial { slot, dial } => self.chat_widget.confirm_quick_dial(slot, dial),
            AppEvent::SaveQuickDial { slot, dial } => {
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.save_quick_dial(slot, dial).await;
            }
            AppEvent::RecallQuickDial { slot, thread_id } => {
                if self.chat_widget.thread_id() == thread_id {
                    self.chat_widget.recall_quick_dial(slot);
                }
            }
            AppEvent::CyberModelAutoReviewNotice => {
                self.chat_widget.add_warning_message(
                    "Cyber models default to \"Approve for me\" for safety reasons.".to_string(),
                );
            }
            AppEvent::PluginUninstallLoaded {
                cwd,
                plugin_id: _plugin_id,
                plugin_display_name,
                result,
            } => {
                let uninstall_succeeded = result.is_ok();
                if uninstall_succeeded {
                    self.refresh_plugin_mentions_after_config_write();
                }
                self.chat_widget.on_plugin_uninstall_loaded(
                    cwd.clone(),
                    plugin_display_name,
                    result,
                );
                if uninstall_succeeded
                    && self.chat_widget.config_ref().cwd.as_path() == cwd.as_path()
                {
                    self.fetch_plugins_list(app_server, cwd);
                }
            }
            AppEvent::RefreshPluginMentions => {
                self.refresh_plugin_mentions(app_server);
            }
            AppEvent::PluginMentionsLoaded { mut plugins, .. } => {
                if !self.config.features.enabled(Feature::Plugins) {
                    plugins = None;
                }
                self.chat_widget.on_plugin_mentions_loaded(plugins);
            }
            AppEvent::OpenRealtimeSettings => self.chat_widget.open_realtime_settings(),
            AppEvent::OpenRealtimeSoundDevices => self.chat_widget.open_realtime_sound_devices(),
            AppEvent::OpenRealtimeVoices => self.open_realtime_voices(app_server),
            AppEvent::OpenRealtimeDevicePicker { kind } => self.list_realtime_devices(kind),
            AppEvent::OpenRealtimeInputChannels { device } => self.chat_widget.open_realtime_input_channels(device),
            AppEvent::RealtimeDevicesListed { origin, kind, result } => {
                if origin == self.active_thread_id {
                    match result {
                        Ok(devices) => self.chat_widget.open_realtime_device_picker(kind, devices),
                        Err(error) => self.chat_widget.add_error_message(error),
                    }
                }
            }
            AppEvent::PersistRealtimeDevice { kind, name } => {
                self.persist_realtime_device(kind, name).await;
            }
            AppEvent::PersistRealtimeInputChannel { channel } => {
                self.persist_realtime_input_channel(channel).await;
            }
            AppEvent::PersistRealtimeVoiceSelection { voice } => {
                self.persist_realtime_voice(app_server, voice);
            }
            AppEvent::ServiceTierSelected { service_tier } => self.select_service_tier(service_tier).await,

            AppEvent::UpdateAskForApprovalPolicy(policy) => {
                if self.reject_pending_permission_change() {
                    return Ok(AppRunControl::Continue);
                }
                let mut config = self.config.clone();
                if !self.try_set_approval_policy_on_config(
                    &mut config,
                    policy,
                    "Failed to set approval policy",
                    "failed to set approval policy on app config",
                ) {
                    return Ok(AppRunControl::Continue);
                }
                self.config = config;
                let approval_policy =
                    AskForApproval::from(self.config.permissions.approval_policy.value());
                self.runtime_approval_policy_override =
                    Some(RuntimeApprovalPolicyOverride::Explicit(approval_policy));
                self.chat_widget.set_approval_policy(approval_policy);
                self.sync_active_thread_permission_settings_to_cached_session()
                    .await;
            }
            AppEvent::UpdateActivePermissionProfile(active_permission_profile) => {
                if self.reject_pending_permission_change() {
                    return Ok(AppRunControl::Continue);
                }
                let mut config = self.config.clone();
                let Some(permission_profile) = self
                    .try_set_builtin_active_permission_profile_on_config(
                        &mut config,
                        active_permission_profile.clone(),
                        "Failed to set permission profile",
                        "failed to set active permission profile on app config",
                    )
                else {
                    return Ok(AppRunControl::Continue);
                };
                let permission_profile_for_chat = permission_profile.clone();

                self.config = config;
                if let Err(err) = self
                    .chat_widget
                    .set_permission_profile_from_session_snapshot(
                        PermissionProfileSnapshot::active(
                            permission_profile_for_chat,
                            active_permission_profile,
                        ),
                    )
                {
                    tracing::warn!(%err, "failed to set permission profile on chat config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set permission profile: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.runtime_approvals_reviewer_override = Some(self.config.approvals_reviewer);
                self.runtime_permission_profile_override =
                    Some(RuntimePermissionProfileOverride::from_config(&self.config));
                self.sync_active_thread_permission_settings_to_cached_session()
                    .await;
                self.chat_widget.submit_initial_user_message_if_pending();
            }
            AppEvent::SelectPermissionProfile(selection) => {
                let inline_selection = self
                    .chat_widget
                    .complete_inline_permission_profile_selection();
                self.select_permission_profile(app_server, selection).await;
                if inline_selection {
                    self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
                }
            }
            AppEvent::UpdateApprovalsReviewer(policy) => {
                if self.reject_pending_permission_change() {
                    return Ok(AppRunControl::Continue);
                }
                self.runtime_approvals_reviewer_override = Some(policy);
                self.config.approvals_reviewer = policy;
                self.chat_widget.set_approvals_reviewer(policy);
                self.sync_active_thread_permission_settings_to_cached_session()
                    .await;
                if let Err(err) = crate::config_update::write_config_batch(
                    app_server.request_handle(),
                    vec![crate::config_update::replace_config_value(
                        "approvals_reviewer",
                        serde_json::json!(policy.to_string()),
                    )],
                )
                .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist approvals reviewer update"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save approvals reviewer: {}",
                        format_config_error(&err)
                    ));
                }
            }
            AppEvent::FetchExperimentalFeatures { thread_id, response_tx } => {
                self.fetch_experimental_features(app_server, thread_id, response_tx);
            }
            AppEvent::SaveExperimentalFeatures { thread_id, updates, response_tx } => {
                self.save_experimental_features(app_server, thread_id, updates, response_tx);
            }
            AppEvent::EnableFeatureForNewThreads(feature) => {
                self.enable_feature_for_new_threads(tui, app_server, feature)
                    .await;
            }
            AppEvent::UpdateFeatureFlags { updates } => {
                self.update_feature_flags(app_server, updates).await;
            }
            AppEvent::UpdateMemorySettings {
                use_memories,
                generate_memories,
            } => {
                self.update_memory_settings_with_app_server(
                    app_server,
                    use_memories,
                    generate_memories,
                )
                .await;
            }
            AppEvent::ResetMemories => {
                self.reset_memories_with_app_server(app_server).await;
            }
            AppEvent::UpdateRateLimitSwitchPromptHidden(hidden) => {
                self.chat_widget.set_rate_limit_switch_prompt_hidden(hidden);
            }
            AppEvent::UpdatePlanModeReasoningEffort(effort) => {
                self.on_update_plan_mode_reasoning_effort(effort);
                self.sync_active_thread_plan_mode_reasoning_setting(app_server)
                    .await;
            }
            AppEvent::PersistRateLimitSwitchPromptHidden => {
                self.local_settings.notices.hide_rate_limit_model_nudge = Some(true);
                if let Err(err) = ConfigEditsBuilder::for_config_path(self.local_settings.user_config_path.as_path())
                    .set_hide_rate_limit_model_nudge(/*acknowledged*/ true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist rate limit switch prompt preference"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save rate limit reminder preference: {err}"
                    ));
                }
            }
            AppEvent::PersistPlanModeReasoningEffort(effort) => self.persist_plan_reasoning_effort(app_server, effort),

            AppEvent::PersistModelMigrationPromptAcknowledged {
                from_model,
                to_model,
            } => {
                if let Err(err) = ConfigEditsBuilder::for_config_path(self.local_settings.user_config_path.as_path())
                    .record_model_migration_seen(from_model.as_str(), to_model.as_str())
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist model migration prompt acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save model migration prompt preference: {err}"
                    ));
                }
            }
            AppEvent::OpenAgentsOverview => self.request_agents_overview(app_server),
            AppEvent::StartAgentsDaemon => self.start_agents_daemon(app_server),
            AppEvent::AgentsDaemonStarted { result } => match result {
                Ok(()) => self.chat_widget.add_info_message(
                    "Background server started. Run `codex agents` in another terminal; this session remains unchanged.".into(),
                    None,
                ),
                Err(error) => self.chat_widget.add_error_message(format!(
                    "Failed to start the background server: {error}"
                )),
            },
            AppEvent::SearchAgentsOverview(id, limit) => self.search_agents_overview(app_server.request_handle(), id, limit),
            AppEvent::AgentsOverviewSearchPage { id, rows, cursor, ok } => {
                self.apply_agents_overview_search_page(id, rows, cursor, ok);
            }
            AppEvent::ShowMoreAgentsOverview => self.show_more_agents_overview(app_server),
            AppEvent::RefreshViewedThread { backend_id, thread_id, revision } => {
                if app_server.backend_id() == backend_id {
                    app_server.frozen_receipt.take_if(|(id, rev)| *id == thread_id && *rev == revision);
                    self.agents_overview.refresh_thread_ids.insert(thread_id);
                    self.refresh_changed_agents_overview_threads(app_server);
                }
            }
            AppEvent::NewAgentsOverviewSession { cwd } => {
                return Box::pin(self.new_agents_overview_session(tui, app_server, cwd)).await;
            }
            AppEvent::AgentsOverviewThreadsLoaded { request_id, result } => {
                self.apply_agents_overview_thread_refresh(app_server, request_id, result);
            }
            AppEvent::SelectAgentsOverviewThread { thread_id } => {
                match self
                    .select_agents_overview_thread(tui, app_server, thread_id)
                    .await?
                {
                    AppRunControl::Continue
                        if self.primary_thread_id.is_none()
                            && self.chat_widget.selected_index_for_present_view(AGENTS_OVERVIEW_VIEW_ID).is_none() => {
                        self.open_agents_overview(app_server);
                    }
                    AppRunControl::Continue => {}
                    AppRunControl::Exit(reason) => return Ok(AppRunControl::Exit(reason)),
                }
            }
            AppEvent::ForkAgentsOverviewThread { thread_id } => {
                if self.reconnect.offline {
                    return Ok(AppRunControl::Continue);
                }
                if !self.side_threads.is_empty()
                    && self.current_displayed_thread_id() != Some(thread_id)
                {
                    self.add_agents_overview_error(
                        "Close the side conversation before forking another task.".into(),
                    );
                    return Ok(AppRunControl::Continue);
                }
                return self.select_agents_overview_thread_after(
                    tui, app_server, thread_id, super::turn_submissions::SelectionAfter::Fork,
                ).await;
            }
            AppEvent::RenameAgentsOverviewThread { thread_id, name } => {
                self.rename_thread(app_server, thread_id, name, Vec::new());
            }
            AppEvent::ConfirmedThreadName { request, conflict_thread_ids } => {
                match request {
                    super::thread_name::ThreadNameRequest::Rename { thread_id, name } => {
                        self.rename_thread(app_server, thread_id, name, conflict_thread_ids);
                    }
                    super::thread_name::ThreadNameRequest::Fork { thread_id, name } => {
                        if self.chat_widget.thread_id() != Some(thread_id) {
                            self.chat_widget.add_error_message("Fork canceled because the source thread changed.".to_string());
                        } else {
                            self.fork_current_thread(tui, app_server, Some(name), conflict_thread_ids, /*from_agents_overview*/ false);
                            self.chat_widget.maybe_send_next_queued_input();
                        }
                    }
                }
            }
            AppEvent::ToggleAgentsOverviewPin { thread_id, pinned } => {
                self.toggle_agents_overview_pin(app_server, thread_id, pinned);
            }
            AppEvent::AgentsOverviewPinToggled {
                request_id,
                thread_id,
                pinned,
                result,
            } => {
                self.complete_agents_overview_pin(
                    app_server, request_id, thread_id, pinned, result,
                );
            }
            AppEvent::SuggestThreadName {
                thread_id,
                request_id,
            } => {
                self.suggest_thread_name(app_server, thread_id, request_id)
                    .await;
            }
            AppEvent::ThreadTitleStarted {
                cancellation,
                thread_id,
                destination,
                prompt,
                effort,
                result,
            } => {
                self.on_thread_title_started(
                    app_server,
                    thread_id,
                    destination,
                    prompt,
                    effort,
                    result,
                    cancellation,
                );
            }
            AppEvent::GeneratedThreadTitle {
                cancellation,
                thread_id,
                temporary_thread_id,
                destination,
                result,
            } => {
                self.temporary_structured_requests
                    .remove(&temporary_thread_id);

                if cancellation.is_cancelled() {
                    return Ok(AppRunControl::Continue);
                }
                self.finish_thread_title_generation(thread_id, destination);
                if let ThreadTitleDestination::RenameSuggestion { request_id } = destination {
                    let suggestion = result.ok()
                        .and_then(|response| super::thread_title::parse_thread_title(&response));
                    self.chat_widget.apply_thread_name_suggestion(
                        thread_id, request_id, suggestion.as_deref(),
                    );
                }
            }
            AppEvent::HideAgentsOverviewThread { thread_id } => {
                self.prepare_agents_overview_removal(&HashSet::from([thread_id]));
                self.agents_overview.hidden_threads.insert(thread_id);
                self.repaint_agents_overview();
            }
            AppEvent::ConfirmAgentsOverviewAction { thread_id, action } => {
                self.confirm_agents_overview_action(thread_id, action);
            }
            AppEvent::RunAgentsOverviewAction { thread_id, action } => {
                self.run_agents_overview_action(tui, app_server, thread_id, action).await?;
            }
            AppEvent::StopAgentsOverviewThread { thread_id } => {
                self.stop_agents_overview_thread(app_server, thread_id);
            }
            AppEvent::OpenAgentPicker => {
                self.open_agent_picker(app_server).await;
            }
            AppEvent::AgentRetired { root, target, result } => {
                self.finish_agent_retirement(tui, app_server, root, target, result).await?;
            }
            AppEvent::AgentPickerThreadsLoaded {
                primary_thread_id,
                request_id,
                result,
            } => {
                self.apply_agent_picker_thread_refresh(
                    app_server,
                    primary_thread_id,
                    request_id,
                    result,
                );
            }
            AppEvent::SignInRecovery(event) => { self.handle_sign_in_recovery(tui, app_server, event).await?; }
            AppEvent::SelectAgentThread(thread_id) => {
                self.select_agent_thread(tui, app_server, thread_id)
                    .await?;
            }
            AppEvent::RefreshSubagentRoster(root) => {
                if self.primary_thread_id == Some(root) {
                    self.refresh_agent_picker_threads(app_server, root);
                }
            }
            AppEvent::StartSide {
                parent_thread_id,
                user_message,
                mode,
            } => {
                return Box::pin(self.handle_start_side(tui, app_server, parent_thread_id, user_message, mode)).await;
            }
            AppEvent::SideChatRemovalFinished { backend_id, target, request, action, result } => {
                if backend_id == app_server.backend_id()
                    && let Some(side) = self.side_threads.get_mut(&target)
                    && side.close_request == Some(request)
                {
                    side.close_request = None;
                    match (action, result) {
                        (sidebar_close::Action::Archive, Ok(())) => {
                            sidebar_close::forget_archived(self, tui, app_server, target).await?;
                        }
                        (sidebar_close::Action::CloseTab, Ok(())) => {
                            self.abandoned_side_threads.insert(target);
                            self.discard_thread_local_state(target).await;
                        }
                        (_, Err(message)) => {
                            if action == sidebar_close::Action::Archive {
                                // The server may have stopped the writer before the file move failed.
                                if let Some(channel) = self.thread_event_channels.get_mut(&target) {
                                    channel.mark_replay_only();
                                }
                                self.mark_agent_picker_thread_closed(target);
                            }
                            self.add_agents_overview_error(message);
                        }
                    }
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::SideChatArchived { backend_id, target } => {
                if backend_id == app_server.backend_id() {
                    sidebar_close::forget_archived(self, tui, app_server, target).await?;
                }
            }
            AppEvent::SavedChatPrepared { target, request, result } => {
                Box::pin(self.finish_saved_chat(tui, app_server, target, request, *result)).await?;
            }
            AppEvent::SavedChatReleased { target, request } => {
                self.finish_saved_release(target, request);
            }
            AppEvent::StartQueuedSavedChat => {
                if let Some(target) = self.waiting_saved_chat()
                    && target.backend_id == app_server.backend_id()
                    && self.wants_saved_chat(target)
                {
                    Box::pin(self.open_saved_chat(tui, app_server, target)).await?;
                }
            }
            AppEvent::SavedSideChatsLoaded { backend_id, parent, request, result } => {
                use crate::app_event::SavedSideChatData;
                use crate::sidebar_chooser::Discovery;
                if let Some(pane) = self.sidebars.get_mut(&Some(parent))
                    && pane.discovery.request() == Some(request)
                {
                    match result {
                        _ if backend_id != app_server.backend_id() => pane.set_discovery(Discovery::Idle),
                        Err(error) => pane.set_discovery(Discovery::Failed(error)),
                        Ok(SavedSideChatData::LatestPrompts(prompts)) => {
                            pane.latest_prompts = prompts;
                            pane.set_discovery(Discovery::Idle);
                        }
                        Ok(SavedSideChatData::Choices(chats)) => {
                            pane.update_saved_chats(chats);
                            self.refresh_visible_side_chat_prompts(tui, app_server, parent, request);
                        }
                    }
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::Farm { thread_id, mode, submission } => {
                self.start_farm(tui, app_server, thread_id, mode, submission).await;
            }
            AppEvent::OpenJira => self.open_jira(tui, app_server),
            AppEvent::JiraFinished { token, result } => self.finish_jira(tui, app_server, token, result).await,
            AppEvent::FarmFinished { request_id, result } => {
                self.finish_farm(tui, request_id, result).await;
            }
            AppEvent::OpenFarmRecovery { thread_id } => self.open_farm_recovery(thread_id),
            AppEvent::ReconcileFarm { request_id } => self.reconcile_farm(tui, app_server, &request_id).await,
            AppEvent::DiscardFarm { request_id } => self.discard_farm(tui, &request_id).await,
            AppEvent::RestoreFarmDraft { request_id } => self.restore_farm_draft(&request_id),
            AppEvent::RestoreFarmReceipt { thread_id, request_id } => self.restore_farm_receipt(tui, thread_id, &request_id),
            AppEvent::OpenSkillsList => {
                self.chat_widget.open_skills_list();
            }
            AppEvent::OpenManageSkillsPopup => {
                self.chat_widget.open_manage_skills_popup();
            }
            AppEvent::SetSkillEnabled { path, enabled } => {
                match crate::config_update::write_skill_enabled(
                    app_server.request_handle(),
                    path.clone(),
                    enabled,
                )
                .await
                {
                    Ok(()) => {
                        self.chat_widget.update_skill_enabled(path, enabled);
                    }
                    Err(err) => {
                        let path_display = path.inferred_native_path_string();
                        self.chat_widget.add_error_message(format!(
                            "Failed to update skill config for {path_display}: {err}"
                        ));
                    }
                }
            }
            AppEvent::SetAppEnabled { id, enabled } => {
                let edits = if enabled {
                    vec![
                        crate::config_update::clear_config_value(
                            crate::config_update::app_scoped_key_path(&id, "enabled"),
                        ),
                        crate::config_update::clear_config_value(
                            crate::config_update::app_scoped_key_path(&id, "disabled_reason"),
                        ),
                    ]
                } else {
                    vec![
                        crate::config_update::replace_config_value(
                            crate::config_update::app_scoped_key_path(&id, "enabled"),
                            serde_json::json!(false),
                        ),
                        crate::config_update::replace_config_value(
                            crate::config_update::app_scoped_key_path(&id, "disabled_reason"),
                            serde_json::json!("user"),
                        ),
                    ]
                };
                match crate::config_update::write_config_batch(app_server.request_handle(), edits)
                    .await
                {
                    Ok(_) => {
                        self.chat_widget.update_connector_enabled(&id, enabled);
                    }
                    Err(err) => {
                        self.chat_widget.add_error_message(format!(
                            "Failed to update app config for {id}: {err}"
                        ));
                    }
                }
            }
            AppEvent::SetHookEnabled { key, enabled } => {
                self.set_hook_enabled(app_server, key, enabled);
            }
            AppEvent::TrustHook { key, current_hash } => {
                self.trust_hook(app_server, key, current_hash);
            }
            AppEvent::TrustHooks { updates } => {
                self.trust_hooks(app_server, updates);
            }
            AppEvent::HookEnabledSet {
                key,
                enabled,
                result,
            } => {
                let queued_enabled = self
                    .pending_hook_enabled_writes
                    .get_mut(&key)
                    .and_then(Option::take);
                let should_apply_result = if let Some(queued_enabled) = queued_enabled
                    && (result.is_err() || queued_enabled != enabled)
                {
                    self.spawn_hook_enabled_write(app_server, key.clone(), queued_enabled);
                    false
                } else {
                    true
                };
                if should_apply_result {
                    self.pending_hook_enabled_writes.remove(&key);
                    if let Err(err) = result {
                        self.chat_widget.add_error_message(err);
                    }
                }
            }
            AppEvent::HookTrusted { result } => {
                if let Err(err) = result {
                    self.chat_widget.add_error_message(err);
                }
            }
            AppEvent::ResolvePermissionProfile { profile_id } => {
                if self.reject_pending_permission_change() {
                    self.chat_widget
                        .retain_input_after_failed_permission_selection();
                    self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
                    return Ok(AppRunControl::Continue);
                }
                #[cfg(any(target_os = "windows", test))]
                if self.chat_widget.windows_sandbox_local_server
                    && self.windows_sandbox_host() != WindowsSandboxHost::Remote
                    && self.chat_widget.windows_sandbox_config.requirements.is_none()
                    && !self.refresh_windows_sandbox_config(app_server).await
                {
                    self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
                    return Ok(AppRunControl::Continue);
                }
                self.chat_widget
                    .select_permission_profile(&profile_id);
            }
            AppEvent::OpenPermissionsPopup | AppEvent::OpenApprovalsPopup => {
                self.chat_widget.permission_popup_dispatched();
                if self.reject_pending_permission_change() {
                    self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
                    return Ok(AppRunControl::Continue);
                }
                #[cfg(any(target_os = "windows", test))]
                if self.chat_widget.windows_sandbox_local_server
                    && self.windows_sandbox_host() != WindowsSandboxHost::Remote
                    && self.chat_widget.windows_sandbox_config.requirements.is_none()
                    && !self.refresh_windows_sandbox_config(app_server).await
                {
                    self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
                    return Ok(AppRunControl::Continue);
                }
                if app_server.uses_remote_workspace() {
                    self.chat_widget.request_permission_profiles();
                } else {
                    self.chat_widget.open_approvals_popup();
                }
            }
            AppEvent::OpenReviewBranchPicker(cwd) => {
                self.chat_widget.show_review_branch_picker(&cwd).await;
            }
            AppEvent::OpenReviewCommitPicker(cwd) => {
                self.chat_widget.show_review_commit_picker(&cwd).await;
            }
            AppEvent::OpenReviewCustomPrompt => {
                self.chat_widget.show_review_custom_prompt();
            }
            AppEvent::SubmitUserMessageWithMode {
                text,
                collaboration_mode,
            } => {
                self.chat_widget
                    .submit_user_message_with_mode(text, collaboration_mode);
            }
            AppEvent::ManageSkillsClosed => {
                self.chat_widget.handle_manage_skills_closed();
            }
            AppEvent::FullScreenUserVerificationRequest(request) => {
                let _ = tui.enter_alt_screen();
                self.overlay = Some(Overlay::new_static_with_renderables(
                    vec![crate::bottom_pane::user_verification::prompt_header(&request)],
                    "U S E R  V E R I F I C A T I O N".to_string(),
                    self.keymap.pager.clone(),
                ));
            }
            AppEvent::FullScreenApprovalRequest(request) => match request {
                ApprovalRequest::ApplyPatch(request) => {
                    let _ = tui.enter_alt_screen();
                    let diff_summary = DiffSummary::new(request.changes, request.cwd);
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![diff_summary.into()],
                        "P A T C H".to_string(),
                        self.keymap.pager.clone(),
                    ));
                }
                ApprovalRequest::Exec(request) => {
                    let _ = tui.enter_alt_screen();
                    let full_cmd = strip_bash_lc_and_escape(&request.command);
                    let full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                    self.overlay = Some(Overlay::new_static_with_lines(
                        full_cmd_lines,
                        "E X E C".to_string(),
                        self.keymap.pager.clone(),
                    ));
                }
                ApprovalRequest::Permissions(request) => {
                    let _ = tui.enter_alt_screen();
                    let mut lines = Vec::new();
                    if let Some(environment_id) = request.environment_id {
                        lines.push(Line::from(vec![
                            "Environment: ".into(),
                            environment_id.bold(),
                        ]));
                        lines.push(Line::from(""));
                    }
                    if let Some(reason) = request.reason {
                        lines.push(Line::from(vec!["Reason: ".into(), reason.italic()]));
                        lines.push(Line::from(""));
                    }
                    if let Some(rule_line) =
                        crate::bottom_pane::format_requested_permissions_rule(&request.permissions)
                    {
                        lines.push(Line::from(vec![
                            "Permission rule: ".into(),
                            rule_line.cyan(),
                        ]));
                    }
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(Paragraph::new(lines).wrap(Wrap { trim: false }))],
                        "P E R M I S S I O N S".to_string(),
                        self.keymap.pager.clone(),
                    ));
                }
                ApprovalRequest::McpElicitation(request) => {
                    let _ = tui.enter_alt_screen();
                    let paragraph = Paragraph::new(vec![
                        Line::from(vec!["Server: ".into(), request.server_name.bold()]),
                        Line::from(""),
                        Line::from(request.message),
                    ])
                    .wrap(Wrap { trim: false });
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(paragraph)],
                        "E L I C I T A T I O N".to_string(),
                        self.keymap.pager.clone(),
                    ));
                }
            },
            AppEvent::StatusLineSetup {
                items,
                use_theme_colors,
            } => {
                let ids = items.iter().map(ToString::to_string).collect::<Vec<_>>();
                let items_edit = crate::legacy_core::config::edit::status_line_items_edit(&ids);
                let colors_edit =
                    crate::legacy_core::config::edit::status_line_use_colors_edit(use_theme_colors);
                let apply_result = ConfigEditsBuilder::for_config_path(self.local_settings.user_config_path.as_path())
                    .with_edits([items_edit, colors_edit])
                    .apply()
                    .await;
                match apply_result {
                    Ok(()) => {
                        self.local_settings.tui.status_line = Some(ids.clone());
                        self.local_settings.tui.status_line_use_colors = use_theme_colors;
                        self.chat_widget.setup_status_line(items, use_theme_colors);
                        self.chat_widget.refresh_config_panel(/*notice*/ None);
                    }
                    Err(err) => {
                        let error = format_config_error(&err);
                        tracing::error!(error = %error, "failed to persist status line settings; keeping previous selection");
                        self.app_event_tx.send(AppEvent::FollowTranscript);
                        let message = format!("Failed to save status line settings: {error}");
                        self.chat_widget.report_config_save_error(message);
                    }
                }
            }
            AppEvent::StatusLineBranchUpdated { cwd, branch } => {
                self.chat_widget.set_status_line_branch(cwd, branch);
                self.refresh_status_line();
            }
            AppEvent::StatusLineGitSummaryUpdated { cwd, summary } => {
                self.chat_widget.set_status_line_git_summary(cwd, summary);
                self.refresh_status_line();
            }
            AppEvent::StatusLineWorkspaceHeadlineUpdated { request_id, result } => {
                if self
                    .chat_widget
                    .set_status_line_workspace_headline(request_id, result)
                {
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::StatusLineSetupCancelled => {
                self.chat_widget.cancel_status_line_setup();
                self.chat_widget.refresh_config_panel(/*notice*/ None);
            }
            AppEvent::TerminalTitleSetup { items } => {
                let ids = items.iter().map(ToString::to_string).collect::<Vec<_>>();
                let edit = crate::legacy_core::config::edit::terminal_title_items_edit(&ids);
                let apply_result = ConfigEditsBuilder::for_config_path(self.local_settings.user_config_path.as_path())
                    .with_edits([edit])
                    .apply()
                    .await;
                match apply_result {
                    Ok(()) => {
                        self.local_settings.tui.terminal_title = Some(ids.clone());
                        self.chat_widget.setup_terminal_title(items);
                        self.chat_widget.refresh_config_panel(/*notice*/ None);
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "failed to persist terminal title items; keeping previous selection");
                        self.app_event_tx.send(AppEvent::FollowTranscript);
                        self.chat_widget.revert_terminal_title_setup_preview();
                        let message = format!("Failed to save terminal title items: {err}");
                        self.chat_widget.report_config_save_error(message);
                    }
                }
            }
            AppEvent::TerminalTitleSetupPreview { items } => {
                self.chat_widget.preview_terminal_title(items);
            }
            AppEvent::TerminalTitleSetupCancelled => {
                self.chat_widget.cancel_terminal_title_setup();
                self.chat_widget.refresh_config_panel(/*notice*/ None);
            }
            AppEvent::Provider(event) => super::provider::handle(self, tui, app_server, event),
            AppEvent::OpenConfigEditor(handoff) => {
                self.cancel_pending_key_chord();
                if let Err(err) = tui.discard_pending_input_before_interactive_screen() {
                    tracing::warn!(%err, "failed to discard input before opening config editor");
                    self.chat_widget.config_editor_open_failed(format!(
                        "Failed to open setting because terminal input could not be cleared: {err}"
                    ));
                } else {
                    let resume_current_availability =
                        super::resume_config::resume_current_directory_availability(
                            &self.app_server_target,
                            self.environment_manager.as_ref(),
                            self.runtime_working_directory_override
                                .as_deref()
                                .or(self.harness_overrides.cwd.as_deref())
                                .or_else(|| app_server.remote_cwd_override()),
                        );
                    self.chat_widget
                        .open_config_editor(handoff, resume_current_availability);
                    self.chat_widget.refresh_config_panel(/*notice*/ None);
                }
            }
            AppEvent::TestConfigNotification => self.schedule_config_notification_test(),
            AppEvent::SendConfigTestNotification { generation } => {
                self.send_config_notification_test(tui, generation)
            }
            AppEvent::SaveConfigPreference(preference) => {
                self.save_config_preference(tui, preference).await;
            }
            AppEvent::PersistAgentsOverviewGrouping(grouping) => {
                self.persist_agents_overview_grouping(grouping).await;
            }
            // TELOS(warpaint): confirmation saves a family override; previews never write.
            AppEvent::SyntaxThemeSelected { name } => {
                self.save_family_theme(app_server, Some(name));
            }
            AppEvent::SyntaxThemeFollowConfig => {
                self.save_family_theme(app_server, None);
            }
            AppEvent::SyntaxThemeFollowConfigPreview => self.preview_config_theme(),
            AppEvent::ResolveTheme => {
                self.refresh_family_theme(app_server);
            }
            AppEvent::SyntaxThemePreviewed => {
                self.resolve_runtime_theme();
                self.refresh_status_line();
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenKeymapActionMenu { context, action } => {
                self.chat_widget
                    .open_keymap_action_menu(context, action, &self.keymap);
            }
            AppEvent::OpenKeymapReplaceBindingMenu { context, action } => {
                self.chat_widget
                    .open_keymap_replace_binding_menu(context, action, &self.keymap);
            }
            AppEvent::OpenKeymapCapture {
                context,
                action,
                intent,
                capture_mode,
            } => {
                self.chat_widget.open_keymap_capture(
                    context,
                    action,
                    intent,
                    capture_mode,
                    &self.keymap,
                );
            }
            AppEvent::OpenKeymapDebug => {
                self.chat_widget.open_keymap_debug(&self.keymap);
            }
            AppEvent::KeymapCaptured {
                context,
                action,
                key,
                intent,
            } => {
                self.apply_keymap_capture(app_server, context, action, key, intent)
                    .await;
                self.merge_startup_warnings(tui, &history_cell::StartupWarningsCell::default());
            }
            AppEvent::KeymapCleared { context, action } => {
                self.apply_keymap_clear(context, action).await;
                self.merge_startup_warnings(tui, &history_cell::StartupWarningsCell::default());
            }
            AppEvent::GenerateRecap { thread_id } => {
                if self.current_displayed_thread_id() == Some(thread_id) {
                    if self.chat_widget.is_user_turn_pending_or_running() {
                        self.chat_widget.add_error_message(
                            "Wait for the current task to finish before running /recap.".to_string(),
                        );
                    } else {
                        self.request_recap(app_server, thread_id, RecapTrigger::Manual);
                    }
                }
            }
            AppEvent::CheckRecap { thread_id } => {
                if self.current_displayed_thread_id() == Some(thread_id)
                    && !self.chat_widget.is_user_turn_pending_or_running()
                    && self
                        .recap
                        .should_generate(tokio::time::Instant::now().into_std())
                {
                    self.request_recap(app_server, thread_id, RecapTrigger::Automatic);
                }
            }
            AppEvent::RecapStarted {
                thread_id,
                request_id,
                trigger,
                completed_turn_count,
                turn_revision,
                history,
                result,
            } => {
                self.handle_recap_started(
                    app_server,
                    recap::RecapRequest {
                        thread_id,
                        request_id,
                        trigger,
                        completed_turn_count,
                        turn_revision,
                    },
                    history,
                    result,
                );
            }
            AppEvent::RecapGenerated {
                thread_id,
                request_id,
                trigger,
                temporary_thread_id,
                completed_turn_count,
                turn_revision,
                result,
            } => {
                self.temporary_structured_requests.remove(&temporary_thread_id);
                if let Some(cell) = self.handle_generated_recap(
                    recap::RecapRequest {
                        thread_id,
                        request_id,
                        trigger,
                        completed_turn_count,
                        turn_revision,
                    },
                    temporary_thread_id,
                    result,
                ) {
                    self.insert_history_cell(tui, Box::new(cell));
                }
            }
        }
        if let Some(model) = sparkle_model {
            self.chat_widget
                .on_sparkle_model_selected_from_picker(&model);
        }
        Ok(AppRunControl::Continue)
    }

    async fn apply_keymap_capture(
        &mut self,
        app_server: &mut AppServerSession,
        context: String,
        action: String,
        key: String,
        intent: crate::app_event::KeymapEditIntent,
    ) {
        let outcome = match crate::keymap_setup::keymap_with_edit(
            &self.local_settings.tui.keymap,
            &self.keymap,
            &context,
            &action,
            &key,
            &intent,
        ) {
            Ok(outcome) => outcome,
            Err(err) => {
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.chat_widget.add_error_message(err);
                return;
            }
        };
        let (keymap_config, bindings, message) = match outcome {
            crate::keymap_setup::KeymapEditOutcome::Updated {
                keymap_config,
                bindings,
                message,
            } => (*keymap_config, bindings, message),
            crate::keymap_setup::KeymapEditOutcome::Unchanged { message } => {
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.chat_widget.add_info_message(message, /*hint*/ None);
                return;
            }
        };

        let runtime_keymap = match RuntimeKeymap::for_session(
            &keymap_config,
            self.local_settings.workspace_enabled(&self.config),
        ) {
            Ok(runtime_keymap) => runtime_keymap,
            Err(err) => {
                let params = crate::keymap_setup::build_keymap_conflict_params(
                    context,
                    action,
                    key,
                    intent,
                    err,
                    &self.keymap,
                );
                self.chat_widget.show_selection_view(params);
                return;
            }
        };

        let save = if matches!(self.app_server_target, AppServerTarget::LocalDaemon { .. }) {
            // Let the daemon validate shared config before writing: older daemons may
            // not understand bindings introduced by this TUI, including new defaults.
            app_server
                .request_handle()
                .request_typed::<codex_app_server_protocol::ConfigWriteResponse>(
                    codex_app_server_protocol::ClientRequest::ConfigValueWrite {
                        request_id: app_server.next_request_id(),
                        params: codex_app_server_protocol::ConfigValueWriteParams {
                            key_path: format!("tui.keymap.{context}.{action}"),
                            value: match bindings.as_slice() {
                                [binding] => serde_json::json!(binding),
                                bindings => serde_json::json!(bindings),
                            },
                            merge_strategy: codex_app_server_protocol::MergeStrategy::Replace,
                            file_path: Some(
                                self.local_settings.user_config_path.display().to_string(),
                            ),
                            expected_version: None,
                        },
                    },
                )
                .await
                .map(|_| ())
                .map_err(anyhow::Error::new)
        } else {
            let edit = crate::legacy_core::config::edit::keymap_bindings_edit(
                &context, &action, &bindings,
            );
            ConfigEditsBuilder::for_config_path(self.local_settings.user_config_path.as_path())
                .with_edits([edit])
                .apply()
                .await
        };
        match save {
            Ok(()) => {
                self.cancel_pending_key_chord();
                self.local_settings.tui.keymap = keymap_config.clone();
                self.keymap = runtime_keymap.clone();
                self.chat_widget
                    .apply_keymap_update(keymap_config, &runtime_keymap);
                self.sync_side_thread_ui();
                self.chat_widget
                    .return_to_keymap_picker(&context, &action, &runtime_keymap);
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.chat_widget.add_info_message(message, /*hint*/ None);
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to persist keymap binding");
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.chat_widget
                    .report_config_save_error(format!("Failed to save shortcut: {err}"));
            }
        }
    }

    fn refresh_plugin_mentions_after_config_write(&mut self) {
        self.chat_widget.refresh_plugin_mentions();
        self.chat_widget.submit_op(AppCommand::reload_user_config());
        self.chat_widget
            .refresh_connector_mentions(/*force_refresh*/ true);
    }

    async fn apply_keymap_clear(&mut self, context: String, action: String) {
        let keymap_config = match crate::keymap_setup::keymap_without_custom_binding(
            &self.local_settings.tui.keymap,
            &context,
            &action,
        ) {
            Ok(keymap_config) => keymap_config,
            Err(err) => {
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.chat_widget.add_error_message(err);
                return;
            }
        };

        let runtime_keymap = match RuntimeKeymap::for_session(
            &keymap_config,
            self.local_settings.workspace_enabled(&self.config),
        ) {
            Ok(runtime_keymap) => runtime_keymap,
            Err(err) => {
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.chat_widget
                    .add_error_message(format!("Failed to refresh shortcuts: {err}"));
                return;
            }
        };

        let edit = crate::legacy_core::config::edit::keymap_binding_clear_edit(&context, &action);
        match ConfigEditsBuilder::for_config_path(self.local_settings.user_config_path.as_path())
            .with_edits([edit])
            .apply()
            .await
        {
            Ok(()) => {
                self.cancel_pending_key_chord();
                self.local_settings.tui.keymap = keymap_config.clone();
                self.keymap = runtime_keymap.clone();
                self.chat_widget
                    .apply_keymap_update(keymap_config, &runtime_keymap);
                self.sync_side_thread_ui();
                self.chat_widget
                    .return_to_keymap_picker(&context, &action, &runtime_keymap);
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.chat_widget.add_info_message(
                    format!("Removed custom shortcut for `{context}.{action}`."),
                    /*hint*/ None,
                );
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to clear keymap binding");
                self.app_event_tx.send(AppEvent::FollowTranscript);
                self.chat_widget
                    .report_config_save_error(format!("Failed to remove shortcut: {err}"));
            }
        }
    }

    pub(super) async fn handle_exit_mode(
        &mut self,
        app_server: &mut AppServerSession,
        mode: ExitMode,
    ) -> AppRunControl {
        self.supersede_pending_navigation();
        self.drain_local_navigation_recovery().await;
        let reason = if mode == ExitMode::ShutdownAfterInterrupt {
            ExitReason::TurnInterrupted
        } else {
            ExitReason::UserRequested
        };
        let scope = if mode == ExitMode::Immediate {
            super::turn_submissions::DetachScope::VoiceOnly
        } else {
            super::turn_submissions::DetachScope::Exit
        };
        let plan = self.prepare_detach_plan(scope).await;
        if plan.is_empty() {
            return AppRunControl::Exit(reason);
        }
        let origin = self
            .active_thread_id
            .or(self.chat_widget.thread_id())
            .or_else(|| plan.origin());
        let Some(thread_id) = origin else {
            return AppRunControl::Exit(reason);
        };
        self.pending_shutdown_exit_thread_id =
            self.active_thread_id.or(self.chat_widget.thread_id());
        self.enqueue_server_operation(
            app_server,
            thread_id,
            super::turn_submissions::ServerOperation::Control(
                super::turn_submissions::ControlOperation::DetachAndExit { plan, reason },
            ),
        );
        AppRunControl::Continue
    }

    pub(super) async fn archive_current_thread(
        &mut self,
        _tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
    ) -> Result<AppRunControl> {
        Ok(self.enqueue_current_thread_removal(app_server, AgentsOverviewAction::Archive))
    }

    pub(super) async fn delete_current_thread(
        &mut self,
        _tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
    ) -> Result<AppRunControl> {
        Ok(self.enqueue_current_thread_removal(app_server, AgentsOverviewAction::Delete))
    }
}

impl App {
    async fn has_running_owned_conversation(&self) -> bool {
        if self.chat_widget.is_agent_turn_running() {
            return true;
        }
        for (&thread_id, channel) in &self.thread_event_channels {
            if channel.attachment() == ThreadEventAttachment::Live
                && self.active_turn_id_for_thread(thread_id).await.is_some()
            {
                return true;
            }
        }
        false
    }
}
