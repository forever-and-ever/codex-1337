//! Session, resume, fork, and subagent selection lifecycle for the TUI app.
//!
//! This module owns the high-level transitions between app-server threads: starting fresh sessions,
//! resuming/forking saved sessions, replacing ChatWidget instances, and maintaining the agent picker
//! cache used for multi-agent navigation.

use super::agent_picker::AGENT_PICKER_VIEW_ID;
use super::app_server_event_targets::ServerNotificationThreadTarget;
use super::app_server_event_targets::server_notification_thread_target;
use super::app_server_event_targets::server_request_thread_id;
use super::*;
use crate::app_server_session::source_agent_path;
use crate::app_server_session::thread_blocks_direct_input;

#[derive(Clone, Copy)]
pub(super) enum ThreadAttachPresentation {
    /// A primary thread created by thread/start, without inherited history.
    Fresh,
    /// A fresh thread whose startup composer is already visible.
    FreshWithDraft,
    SessionLineage,
}

impl App {
    pub(super) async fn open_agent_picker(&mut self, server: &mut AppServerSession) {
        self.supersede_pending_navigation();
        if self.primary_thread_id.is_none() {
            self.enqueue_agent_picker_discovery(server);
        } else {
            self.show_agent_picker(server);
        }
    }

    pub(super) fn agent_picker_selection_view_params(
        &mut self,
        selected: Option<usize>,
    ) -> SelectionViewParams {
        let selected_thread = selected
            .and_then(|index| self.agent_navigation.picker_order.get(index).copied())
            .or(self.active_thread_id);
        let mut threads = self.agent_navigation.visible_threads();
        if self
            .chat_widget
            .selected_index_for_present_view(AGENT_PICKER_VIEW_ID)
            .is_some()
        {
            threads.sort_by_key(|(id, _)| {
                self.agent_navigation
                    .picker_order
                    .iter()
                    .position(|thread_id| thread_id == id)
                    .unwrap_or(usize::MAX)
            });
        } else {
            threads.sort_by_key(|(id, _)| {
                let status = self.agent_navigation.status(*id);
                (
                    matches!(
                        status,
                        Some(codex_app_server_protocol::ThreadStatus::NotLoaded)
                    ),
                    !matches!(
                        status,
                        Some(codex_app_server_protocol::ThreadStatus::Active { .. })
                    ),
                )
            });
        }
        let picker_order = threads.iter().map(|(id, _)| *id).collect();
        let mut initial_selected_idx = None;
        let items: Vec<SelectionItem> = threads
            .into_iter()
            .enumerate()
            .map(|(idx, (thread_id, entry))| {
                if selected_thread == Some(thread_id) {
                    initial_selected_idx = Some(idx);
                }
                let id = thread_id;
                let is_primary = self.primary_thread_id == Some(thread_id);
                let name = entry
                    .agent_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|agent_path| !is_primary && !agent_path.is_empty())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| {
                        format_agent_picker_item_name(
                            entry.agent_nickname.as_deref(),
                            entry.agent_role.as_deref(),
                            is_primary,
                        )
                    });
                let uuid = thread_id.to_string();
                SelectionItem {
                    name: name.clone(),
                    name_prefix_spans: {
                        vec![
                            crate::agent_status::marker(self.agent_navigation.status(thread_id)),
                            " ".into(),
                        ]
                    },
                    description: Some(uuid.clone()),
                    is_current: self.active_thread_id == Some(thread_id),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SelectAgentThread(id));
                    })],
                    dismiss_on_select: true,
                    search_value: Some(format!("{name} {uuid}")),
                    ..Default::default()
                }
            })
            .collect();
        self.agent_navigation.picker_order = picker_order;

        SelectionViewParams {
            view_id: Some(AGENT_PICKER_VIEW_ID),
            title: Some("Subagents".to_string()),
            subtitle: Some(AgentNavigationState::picker_subtitle()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..SelectionViewParams::picker()
        }
    }

    pub(super) fn is_terminal_thread_read_error(err: &color_eyre::Report) -> bool {
        err.chain()
            .any(|cause| cause.to_string().contains("thread not loaded:"))
    }

    pub(super) fn closed_state_for_thread_read_error(
        err: &color_eyre::Report,
        existing_is_closed: Option<bool>,
    ) -> bool {
        Self::is_terminal_thread_read_error(err) || existing_is_closed.unwrap_or(false)
    }

    pub(super) fn can_fallback_from_include_turns_error(err: &color_eyre::Report) -> bool {
        err.chain().any(|cause| {
            let message = cause.to_string();
            message.contains("includeTurns is unavailable before first user message")
                || message.contains("thread/turns/list is unavailable before first user message")
                || message.contains("ephemeral threads do not support includeTurns")
        })
    }

    /// Updates cached picker metadata and then mirrors any visible-label change into the footer.
    ///
    /// These two writes stay paired so the picker rows and contextual footer continue to describe
    /// the same displayed thread after nickname or role updates.
    pub(super) fn upsert_agent_picker_thread(
        &mut self,
        thread_id: ThreadId,
        agent_nickname: Option<String>,
        agent_role: Option<String>,
        is_closed: bool,
    ) {
        self.chat_widget.set_collab_agent_metadata(
            thread_id,
            agent_nickname.clone(),
            agent_role.clone(),
        );
        self.agent_navigation
            .upsert(thread_id, agent_nickname, agent_role, is_closed);
        self.sync_active_agent_label();
        self.refresh_visible_agent_picker();
    }

    /// Persists the app-server's authoritative ownership flag and updates the active composer.
    pub(super) fn mark_primary_thread_parent_owned(&mut self, thread_id: ThreadId) {
        self.agent_navigation.mark_parent_owned(thread_id);
        self.chat_widget.set_parent_owned_thread();
        if self
            .agent_navigation
            .text_input_threads
            .contains(&thread_id)
        {
            self.chat_widget.set_child_text_input_enabled();
        }
    }

    /// Marks a cached picker thread closed and recomputes the contextual footer label.
    ///
    /// Closing a thread is not the same as removing it: users can still inspect finished agent
    /// transcripts, and the stable next/previous traversal order should not collapse around them.
    pub(super) fn mark_agent_picker_thread_closed(&mut self, thread_id: ThreadId) {
        self.agent_navigation.mark_closed(thread_id);
        self.sync_active_agent_label();
    }

    pub(super) fn apply_thread_liveness(
        &mut self,
        result: Result<codex_app_server_protocol::Thread>,
        thread_id: ThreadId,
    ) -> bool {
        let existing_entry = self.agent_navigation.get(&thread_id).cloned();
        let has_replay_channel = self.thread_event_channels.contains_key(&thread_id);
        match result {
            Ok(thread) => {
                self.agent_navigation.record_model(&thread);
                let is_parent_owned = thread_blocks_direct_input(&thread);
                let agent_path = source_agent_path(&thread.source);
                let is_closed = matches!(
                    thread.status,
                    codex_app_server_protocol::ThreadStatus::NotLoaded
                );
                self.upsert_agent_picker_thread(
                    thread_id,
                    thread.agent_nickname.or_else(|| {
                        existing_entry
                            .as_ref()
                            .and_then(|entry| entry.agent_nickname.clone())
                    }),
                    thread.agent_role.or_else(|| {
                        existing_entry
                            .as_ref()
                            .and_then(|entry| entry.agent_role.clone())
                    }),
                    is_closed,
                );
                if is_parent_owned {
                    self.agent_navigation.mark_parent_owned(thread_id);
                }
                self.agent_navigation.set_agent_path(thread_id, agent_path);
                self.agent_navigation.set_status(thread_id, thread.status);
                self.sync_subagent_roster();
                true
            }
            Err(err) => {
                if Self::is_terminal_thread_read_error(&err) && !has_replay_channel {
                    self.agent_navigation.remove(thread_id);
                    self.sync_subagent_roster();
                    return false;
                }
                let is_closed = Self::closed_state_for_thread_read_error(
                    &err,
                    existing_entry.as_ref().map(|entry| entry.is_closed),
                );
                if let Some(entry) = existing_entry {
                    self.upsert_agent_picker_thread(
                        thread_id,
                        entry.agent_nickname,
                        entry.agent_role,
                        is_closed,
                    );
                } else {
                    self.upsert_agent_picker_thread(
                        thread_id, /*agent_nickname*/ None, /*agent_role*/ None,
                        is_closed,
                    );
                }
                self.agent_navigation
                    .set_running(thread_id, /*is_running*/ false);
                true
            }
        }
    }

    /// Replaces the chat widget, carrying the editor yank and re-seeding collab metadata from the
    /// navigation cache.
    ///
    /// Thread switches reconstruct the `ChatWidget`, which loses the `collab_agent_metadata` map.
    /// This helper copies every known nickname/role from `AgentNavigationState` into the
    /// replacement widget so that replayed collab items render agent names immediately.
    pub(super) fn replace_chat_widget(&mut self, mut chat_widget: ChatWidget) {
        chat_widget.fork_in_progress = self.chat_widget.fork_in_progress;
        if self.chat_widget.session_draft.is_some()
            && !self.reconnect.offline
            && let Some(input) = self.chat_widget.capture_thread_input_state()
            && input.has_unsent_input()
        {
            let mut config = self.chat_widget.config_ref().clone();
            config.model = Some(self.chat_widget.current_model().to_owned());
            config.model_reasoning_effort = self.chat_widget.current_reasoning_effort();
            self.agents_overview.parked_draft =
                Some(Box::new(super::session_draft::ParkedSessionDraft {
                    config,
                    catalog: self.chat_widget.model_catalog(),
                    input,
                }));
        }
        self.pending_mouse_paste = None;
        self.config_notification_test_pending = None;
        if !self.chat_widget.realtime_conversation_is_running() {
            self.retain_realtime_replay_state_before_replace();
        }
        self.commit_animation = None;
        // Transfer the last-written terminal title to the replacement widget
        // so it knows what OSC title is currently displayed. Without this, the
        // new widget would redundantly clear and rewrite the same title, causing
        // a visible flicker in some terminals.
        let previous_terminal_title = self.chat_widget.last_terminal_title.take();
        if chat_widget.last_terminal_title.is_none() {
            chat_widget.last_terminal_title = previous_terminal_title;
        }
        chat_widget.config_panel_tui = self.chat_widget.config_panel_tui.clone();
        chat_widget.remote_connection = self.chat_widget.remote_connection.clone();
        chat_widget.snapshot_local_images = self.app_server_target.uses_remote_workspace();
        chat_widget.windows_sandbox_local_server = self.chat_widget.windows_sandbox_local_server;
        chat_widget.windows_sandbox_host = WindowsSandboxHost::Unknown;
        #[cfg(any(target_os = "windows", test))]
        {
            chat_widget.windows_sandbox_elevated_setup_complete =
                self.chat_widget.windows_sandbox_elevated_setup_complete;
        }
        chat_widget.set_agents_navigation_enabled(matches!(
            self.app_server_target,
            AppServerTarget::LocalDaemon { .. }
        ));
        chat_widget.inherit_backend_banner_state(&mut self.chat_widget);
        chat_widget.inherit_security_setup(&mut self.chat_widget);
        for (thread_id, entry) in self.agent_navigation.ordered_threads() {
            chat_widget.set_collab_agent_metadata(
                thread_id,
                entry.agent_nickname.clone(),
                entry.agent_role.clone(),
            );
        }
        chat_widget.restore_kill_buffer_snapshot(self.chat_widget.take_kill_buffer_snapshot());
        crate::markdown_render::preferences::init(chat_widget.local_settings.tui.rendering);
        let mut previous = std::mem::replace(&mut self.chat_widget, chat_widget);
        if previous.realtime_conversation_is_running() {
            previous.park_voice();
            self.background_voice = Some(Box::new(previous));
        }
        self.sync_active_agent_label();
        self.refresh_submission_preview();
    }

    pub(super) async fn select_agent_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<()> {
        self.select_agent_thread_after(
            tui,
            app_server,
            thread_id,
            super::turn_submissions::SelectionAfter::SelectOnly,
        )
        .await
    }

    pub(super) async fn select_agent_thread_after(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        after: super::turn_submissions::SelectionAfter,
    ) -> Result<()> {
        if self.active_thread_id == Some(thread_id) && !self.thread_unavailable(thread_id) {
            self.supersede_pending_navigation();
            return self.finish_selection_after(tui, app_server, after).await;
        }
        if self.reject_pending_permission_root_switch() {
            return Ok(());
        }
        if self.windows_sandbox_blocks_thread_switch() {
            self.chat_widget.add_info_message(
                "Finish Windows sandbox setup before switching threads.".to_string(),
                /*hint*/ None,
            );
            return Ok(());
        }

        if self.side_threads.contains_key(&thread_id)
            && !self.abandoned_side_threads.contains(&thread_id)
            && self.thread_event_channels.contains_key(&thread_id)
        {
            return self
                .select_tracked_side_locally(tui, app_server, thread_id, after)
                .await;
        }
        self.enqueue_thread_selection(app_server, thread_id, after);
        Ok(())
    }

    pub(super) fn render_thread_snapshot(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &AppServerSession,
        thread_id: ThreadId,
        snapshot: ThreadEventSnapshot,
        resume_restored_queue: bool,
    ) -> Result<()> {
        let init = self.chatwidget_init_for_forked_or_resumed_thread(
            tui,
            self.config.clone(),
            /*initial_user_message*/ None,
        );
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
        self.chat_widget
            .set_task_mentions_enabled(app_server.task_tools_available(thread_id));
        self.chat_widget
            .note_rendered_width(tui.terminal.last_known_screen_size.width);
        let external_writer = self
            .thread_event_channels
            .get(&thread_id)
            .is_some_and(|channel| channel.attachment() == ThreadEventAttachment::ExternalWriter);
        if external_writer {
            self.chat_widget.show_external_writer_thread();
        }
        if self.agent_navigation.is_parent_owned(thread_id) {
            self.chat_widget.set_parent_owned_thread();
            if self
                .agent_navigation
                .text_input_threads
                .contains(&thread_id)
            {
                self.chat_widget.set_child_text_input_enabled();
            }
        }
        self.reset_for_thread_switch(tui)?;
        self.pending_thread_switch_resets += 1;
        self.app_event_tx
            .send(AppEvent::ResetTranscriptForThreadSwitch);
        self.replay_thread_snapshot(snapshot, resume_restored_queue);
        if let Some(thread_id) = self.chat_widget.thread_id()
            && let Some(active) = self
                .chat_widget
                .config_ref()
                .permissions
                .active_permission_profile()
            && self
                .agents_overview
                .selected_permission_profiles
                .get(&thread_id)
                == Some(&active.id)
        {
            self.adopt_server_permissions();
        }
        if external_writer {
            self.chat_widget.show_external_writer_thread();
        }
        Ok(())
    }

    pub(super) fn should_attach_live_thread_for_selection(&self, thread_id: ThreadId) -> bool {
        if self
            .agent_navigation
            .get(&thread_id)
            .is_some_and(|entry| entry.is_closed)
        {
            return false;
        }
        self.thread_event_channels
            .get(&thread_id)
            .is_none_or(|channel| channel.attachment() != ThreadEventAttachment::Live)
    }

    pub(super) fn reset_for_thread_switch(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.reset_transcript_state_after_clear();
        tui.clear_pending_history_lines();
        Ok(tui.clear_for_thread_switch()?)
    }

    pub(super) async fn reset_thread_event_state(&mut self) {
        let voice_owner = self.voice_owner_thread_id();
        let mut previous_pending = std::mem::take(&mut self.pending_app_server_requests);
        // Move retained tasks' approvals to background routing before clearing request bookkeeping.
        for (thread_id, requests) in &mut self.agents_overview.dispatched_requests {
            if let Some(channel) = self.thread_event_channels.get(thread_id) {
                for request in channel.store.lock().await.pending_replay_requests() {
                    if !requests
                        .iter()
                        .any(|pending| pending.request.id() == request.id())
                        && let Some(request) = previous_pending.buffered_request(request)
                    {
                        requests.push(request);
                    }
                }
            }
        }
        if voice_owner.is_some() {
            for (thread_id, channel) in &self.thread_event_channels {
                if Some(*thread_id) != voice_owner && !self.side_threads.contains_key(thread_id) {
                    for request in channel.store.lock().await.pending_replay_requests() {
                        previous_pending.resolve_notification(&thread_id.to_string(), request.id());
                    }
                }
            }
        }
        self.thread_event_listener_tasks.retain(|id, task| {
            if Some(*id) == voice_owner
                || self.side_threads.contains_key(id)
                || self.agents_overview.dispatched_requests.contains_key(id)
            {
                true
            } else {
                task.abort();
                false
            }
        });
        self.thread_event_channels
            .retain(|id, _| Some(*id) == voice_owner || self.side_threads.contains_key(id));
        self.pending_realtime_speech_replay.clear();
        self.pending_realtime_transcript_replay.clear();
        self.realtime_replay_order.clear();
        self.pending_server_profiles.clear();
        self.agents_overview.activity.clear();
        self.agent_navigation.clear();
        self.active_thread_id = None;
        self.active_thread_rx = None;
        self.primary_thread_id = None;
        self.last_subagent_backfill_attempt = None;
        self.primary_session_configured = None;
        self.pending_primary_events.clear();
        if voice_owner.is_none() {
            let side_thread_ids: Vec<_> = self.side_threads.keys().copied().collect();
            for thread_id in side_thread_ids {
                if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                    for request in channel.store.lock().await.pending_replay_requests() {
                        if let Some(request) = previous_pending.buffered_request(request) {
                            let _ = self
                                .pending_app_server_requests
                                .note_buffered_request(&request);
                        }
                    }
                }
            }
        } else {
            self.pending_app_server_requests = previous_pending;
        }
        self.pending_startup_thread_start = false;
        self.pending_server_version_notice = None;
        self.chat_widget.set_pending_thread_approvals(Vec::new());
        self.sync_active_agent_label();
    }

    pub(super) async fn handle_startup_thread_started(
        &mut self,
        app_server: &mut AppServerSession,
        result: Result<AppServerStartedThread>,
    ) -> Result<()> {
        if !self.pending_startup_thread_start {
            if let Ok(started) = result {
                let thread_id = started.session.thread_id;
                if let Err(err) = app_server.thread_unsubscribe(thread_id).await {
                    tracing::warn!(
                        thread_id = %thread_id,
                        "failed to unsubscribe stale startup thread: {err}"
                    );
                }
                self.discard_thread_local_state(thread_id).await;
            }
            return Ok(());
        }

        self.pending_startup_thread_start = false;
        self.chat_widget
            .set_queue_submissions_until_session_configured(/*queue*/ false);
        match result {
            Ok(started) => {
                self.chat_widget.mark_fresh_task_for_sparkle(&started);
                let thread_id = started.session.thread_id;
                if started.task_tools_available {
                    self.chat_widget.set_task_mentions_enabled(/*enabled*/ true);
                }
                self.pending_primary_events.retain(|event| match event {
                    ThreadBufferedEvent::Notification(notification) => matches!(
                        server_notification_thread_target(notification),
                        ServerNotificationThreadTarget::Thread(event_thread_id)
                            if event_thread_id == thread_id
                    ),
                    ThreadBufferedEvent::Request(request) => {
                        server_request_thread_id(request) == Some(thread_id)
                    }
                    ThreadBufferedEvent::HistoryEntryResponse(_)
                    | ThreadBufferedEvent::FeedbackSubmission(_) => true,
                    ThreadBufferedEvent::FarmReceipt {
                        thread_id: origin, ..
                    } => *origin == thread_id,
                });
                let previous_pending = std::mem::take(&mut self.pending_app_server_requests);
                for event in &self.pending_primary_events {
                    if let ThreadBufferedEvent::Request(request) = event {
                        let retained = previous_pending.buffered_request((**request).clone());
                        #[cfg(test)]
                        let retained = retained.or_else(|| {
                            Some(super::app_server_requests::BufferedServerRequest::new(
                                (**request).clone(),
                                &app_server.request_handle(),
                            ))
                        });
                        if let Some(request) = retained {
                            let _ = self
                                .pending_app_server_requests
                                .note_buffered_request(&request);
                        }
                    }
                }
                if started.can_accept_text_input {
                    self.agent_navigation
                        .text_input_threads
                        .insert(started.session.thread_id);
                } else {
                    self.agent_navigation
                        .text_input_threads
                        .remove(&started.session.thread_id);
                }
                if started.blocks_direct_input {
                    self.mark_primary_thread_parent_owned(thread_id);
                }
                // Lifecycle notifications may arrive before the thread/start response.
                if !self.config.ephemeral
                    && !matches!(self.app_server_target, AppServerTarget::Embedded)
                    && !self.pending_primary_events.iter().any(|event| {
                        matches!(event, ThreadBufferedEvent::Notification(notification)
                            if matches!(notification.as_ref(),
                                ServerNotification::TurnStarted(_)
                                    | ServerNotification::ThreadClosed(_)
                                    | ServerNotification::ThreadArchived(_)
                                    | ServerNotification::ThreadDeleted(_)))
                    })
                {
                    self.agents_overview
                        .blank_sessions
                        .insert(thread_id, started.clone());
                }
                // A full usage read can finish before thread/start. Apply its cached fallback
                // after attachment but before the initial prompt or queued draft is submitted.
                let recovery_was_pending = self.chat_widget.hold_rate_limit_recovery();
                self.enqueue_primary_thread_session(started.session, started.turns)
                    .await?;
                self.apply_backend_banner_fallback(app_server).await;
                if let Some(notice) = self.pending_server_version_notice.take() {
                    self.chat_widget.add_server_version_warning(notice);
                }
                if !recovery_was_pending {
                    self.chat_widget.finish_rate_limit_recovery();
                }
                self.chat_widget.maybe_send_next_queued_input();
            }
            Err(err) if self.chat_widget.session_draft.is_some() => {
                self.chat_widget.restore_session_draft();
                self.chat_widget.add_error_message(format!(
                    "Could not start the conversation: {err}. Your input was not sent."
                ));
                self.recover_transport_error(&err);
            }
            Err(err) if self.recover_transport_error(&err) => {}
            Err(err) => {
                let warnings = self
                    .transcript_cells
                    .iter()
                    .find_map(|cell| {
                        cell.as_any()
                            .downcast_ref::<history_cell::StartupWarningsCell>()
                    })
                    .filter(|cell| cell.pending_header)
                    .map(|cell| format!("\n\nStartup warnings:\n{}", cell.messages.join("\n")))
                    .unwrap_or_default();
                return Err(color_eyre::eyre::eyre!(
                    "Failed to start a fresh session through the app server: {err}{warnings}"
                ));
            }
        }
        Ok(())
    }

    pub(super) async fn start_fresh_session(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        session_start_source: Option<ThreadStartSource>,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
        new_thread_name: Option<String>,
    ) {
        self.start_fresh_session_with_provider(
            tui,
            app_server,
            session_start_source,
            initial_user_message,
            new_thread_name,
            /*provider*/ None,
        )
        .await;
    }

    pub(super) async fn start_fresh_session_with_provider(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        session_start_source: Option<ThreadStartSource>,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
        new_thread_name: Option<String>,
        provider: Option<super::ProviderModelChoice>,
    ) {
        if self.reject_pending_permission_root_switch() {
            if let Some(message) = initial_user_message {
                self.chat_widget.restore_user_message_to_composer(message);
            }
            return;
        }
        if provider.is_none() && initial_user_message.is_none() && new_thread_name.is_none() {
            match self.restore_parked_session_draft(tui, app_server).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    self.chat_widget
                        .add_error_message(format!("Could not reopen the unsent draft: {error}"));
                    return;
                }
            }
        }
        self.enqueue_fresh_navigation(
            app_server,
            session_start_source,
            initial_user_message,
            new_thread_name,
            provider,
        );
        tui.frame_requester().schedule_frame();
    }

    pub(super) async fn replace_chat_widget_with_app_server_thread(
        &mut self,
        tui: &mut tui::Tui,
        started: AppServerStartedThread,
        presentation: ThreadAttachPresentation,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
    ) -> Result<()> {
        // Initial messages are for freshly attached primary threads only. Thread switches and
        // resume/fork flows pass `None` so they cannot replay old history and then auto-submit a new
        // user turn by accident.
        if let Some(mut receiver) = self.active_thread_rx.take() {
            while let Ok(event) = receiver.try_recv() {
                self.handle_thread_event_now_recovering_file_changes(event)
                    .await;
            }
            self.active_thread_rx = Some(receiver);
        }
        self.store_active_thread_receiver().await;
        if matches!(presentation, ThreadAttachPresentation::FreshWithDraft) {
            self.reset_transcript_state_after_clear();
            tui.clear_pending_history_lines();
            tui.defer_thread_switch_clear();
        } else {
            self.reset_for_thread_switch(tui)?;
        }
        self.pending_thread_switch_resets += 1;
        self.app_event_tx.send(
            if matches!(presentation, ThreadAttachPresentation::FreshWithDraft) {
                AppEvent::ResetTranscriptForThreadSwitchPreservingScreen
            } else {
                AppEvent::ResetTranscriptForThreadSwitch
            },
        );
        self.reset_thread_event_state().await;
        let init = self.chatwidget_init_for_forked_or_resumed_thread(
            tui,
            self.config.clone(),
            initial_user_message,
        );
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
        if matches!(
            presentation,
            ThreadAttachPresentation::Fresh | ThreadAttachPresentation::FreshWithDraft
        ) {
            self.chat_widget.mark_fresh_task_for_sparkle(&started);
            // FreshWithDraft inherits its provisional header and replay at the handoff.
            if matches!(presentation, ThreadAttachPresentation::Fresh) {
                self.chat_widget
                    .empty_state_animation
                    .borrow_mut()
                    .start_fresh();
            }
        }
        self.chat_widget
            .set_task_mentions_enabled(started.task_tools_available);
        self.chat_widget
            .note_rendered_width(tui.terminal.last_known_screen_size.width);
        if started.can_accept_text_input {
            self.agent_navigation
                .text_input_threads
                .insert(started.session.thread_id);
        } else {
            self.agent_navigation
                .text_input_threads
                .remove(&started.session.thread_id);
        }
        if started.blocks_direct_input {
            self.mark_primary_thread_parent_owned(started.session.thread_id);
        }
        self.enqueue_primary_thread_session_with_presentation(
            started.session,
            started.turns,
            presentation,
        )
        .await?;
        if let Some(notice) = started.history_notice {
            self.chat_widget.add_info_message(notice, /*hint*/ None);
        }
        self.sync_frontend_requests();
        Ok(())
    }

    pub(super) async fn resume_target_session(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        target_session: SessionTarget,
    ) -> Result<AppRunControl> {
        self.supersede_pending_navigation();
        if self.ignore_same_thread_resume(&target_session) {
            self.agents_overview
                .hidden_threads
                .remove(&target_session.thread_id);
            self.repaint_agents_overview();
            tui.frame_requester().schedule_frame();
            return Ok(AppRunControl::Continue);
        }
        if self.windows_sandbox_blocks_thread_switch() {
            self.chat_widget.add_info_message(
                "Finish Windows sandbox setup before switching threads.".into(),
                None,
            );
            return Ok(AppRunControl::Continue);
        }
        if self.reject_pending_permission_root_switch() {
            return Ok(AppRunControl::Continue);
        }
        self.enqueue_session_resume(app_server, target_session);
        Ok(AppRunControl::Continue)
    }

    #[cfg(test)]
    pub(super) async fn resume_target_session_and_settle(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        target: SessionTarget,
    ) -> Result<AppRunControl> {
        let control = self.resume_target_session(tui, server, target).await?;
        if matches!(control, AppRunControl::Continue) {
            return self.settle_navigation(tui, server).await;
        }
        Ok(control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_thread_read_error_detection_matches_not_loaded_errors() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: thread not loaded: thr_123"
        );

        assert!(App::is_terminal_thread_read_error(&err));
    }

    #[test]
    fn terminal_thread_read_error_detection_ignores_transient_failures() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read transport error: broken pipe"
        );

        assert!(!App::is_terminal_thread_read_error(&err));
    }

    #[test]
    fn closed_state_for_thread_read_error_preserves_live_state_without_cache_on_transient_error() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read transport error: broken pipe"
        );

        assert!(!App::closed_state_for_thread_read_error(
            &err, /*existing_is_closed*/ None
        ));
    }

    #[test]
    fn closed_state_for_thread_read_error_marks_terminal_uncached_threads_closed() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: thread not loaded: thr_123"
        );

        assert!(App::closed_state_for_thread_read_error(
            &err, /*existing_is_closed*/ None
        ));
    }

    #[test]
    fn include_turns_fallback_detection_handles_unmaterialized_and_ephemeral_threads() {
        let unmaterialized = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: thread thr_123 is not materialized yet; includeTurns is unavailable before first user message"
        );
        let ephemeral = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: ephemeral threads do not support includeTurns"
        );

        assert!(App::can_fallback_from_include_turns_error(&unmaterialized));
        assert!(App::can_fallback_from_include_turns_error(&ephemeral));
    }
}
