//! App-server event stream handling for the TUI app.
//! Hidden structured threads reject requests instead of entering interactive routing.

use super::App;
use super::ThreadBufferedEvent;
use super::app_server_event_targets::ServerNotificationThreadTarget;
use super::app_server_event_targets::server_notification_thread_target;
use super::app_server_event_targets::server_request_thread_id;
use super::app_server_requests::BufferedServerRequest;
use crate::app_event::AppEvent;
use crate::app_event::RateLimitRefreshOrigin;
#[cfg(any(target_os = "windows", test))]
use crate::app_event::WindowsSandboxEnableMode;
use crate::app_info::app_info_from_api;
use crate::app_server_session::AppServerSession;
use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RateLimitReachedType;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::SessionSource;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadSource;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::request_permissions::RequestPermissionProfile as CoreRequestPermissionProfile;

impl App {
    fn reject_unsupported_app_server_request(
        &mut self,
        app_server_client: &AppServerSession,
        unsupported: super::app_server_requests::UnsupportedAppServerRequest,
        request: &BufferedServerRequest,
    ) {
        tracing::warn!(
            request_id = ?unsupported.request_id,
            message = unsupported.message,
            "rejecting unsupported app-server request"
        );
        self.chat_widget
            .add_error_message(unsupported.message.clone());
        self.enqueue_connection_operation(
            app_server_client,
            super::turn_submissions::ServerOperation::Response(Box::new(
                super::turn_submissions::ServerRequestDelivery::Rejected {
                    reply: request.reply.clone(),
                    error: JSONRPCErrorError {
                        code: -32000,
                        message: unsupported.message,
                        data: None,
                    },
                },
            )),
        );
    }

    pub(super) async fn handle_app_server_event(
        &mut self,
        app_server_client: &AppServerSession,
        event: AppServerEvent,
    ) {
        match event {
            AppServerEvent::Lagged { skipped } => {
                tracing::warn!(
                    skipped,
                    "app-server event consumer lagged; dropping ignored events"
                );
                self.chat_widget.finish_mcp_startup_after_lag();
                if let Some(task) = self.agents_overview.refresh_task.take() {
                    task.abort();
                }
                self.agents_overview.request_id = None;
                self.agents_overview.refresh_show_more = false;
                self.agents_overview.active_refresh_thread_ids.clear();
                self.agents_overview.refresh_pending = false;
                self.agents_overview.initialized = false;
                self.agents_overview.refresh_notifications.clear();
                self.agents_overview.activity.clear();
                self.agents_overview.last_messages.clear();
                self.agents_overview.usage.clear();
                self.agents_overview.pending_usage = None;
                self.agents_overview.usage_disabled = false;
                self.repaint_agents_overview();
                self.refresh_agents_overview_threads(app_server_client);
                if let Some(primary_thread_id) = self.primary_thread_id
                    && !self
                        .agent_navigation
                        .queue_picker_refresh(primary_thread_id)
                {
                    self.refresh_agent_picker_threads(app_server_client, primary_thread_id);
                }
            }
            AppServerEvent::ServerNotification(mut notification) => {
                self.note_submission_notification(notification.as_ref());
                self.note_prompt_revert_notification(notification.as_ref());
                if let ServerNotification::ThreadWorkStatusUpdated(update) = notification.as_mut()
                    && update.status.emitted_at.is_none()
                {
                    // Capture the old-server fallback before per-thread buffering, not replay.
                    update.status.emitted_at = Some(chrono::Utc::now().timestamp());
                }
                let request_resolved = matches!(
                    notification.as_ref(),
                    ServerNotification::ServerRequestResolved(_)
                );
                self.handle_server_notification_event(app_server_client, *notification)
                    .await;
                if request_resolved {
                    self.repaint_agents_overview();
                }
            }
            AppServerEvent::ServerRequest(request) => {
                let request =
                    BufferedServerRequest::new(*request, &app_server_client.request_handle());
                self.handle_server_request_event(app_server_client, request)
                    .await;
                self.repaint_agents_overview();
            }
            AppServerEvent::Disconnected { message } => {
                // The request result distinguishes a refused start from a lost reply.
                if self.pending_startup_thread_start
                    && !matches!(self.app_server_target, crate::AppServerTarget::Embedded)
                {
                    return;
                }
                self.cancel_transcript_export();
                if self.begin_reconnect() {
                    return;
                }
                tracing::warn!("app-server event stream disconnected: {message}");
                self.chat_widget.add_error_message(message.clone());
                self.app_event_tx.send(AppEvent::FatalExitRequest(message));
            }
        }
    }

    async fn handle_server_notification_event(
        &mut self,
        app_server_client: &AppServerSession,
        mut notification: ServerNotification,
    ) {
        if let ServerNotification::ThreadRetired(retired) = &notification {
            let ids = retired
                .thread_ids
                .iter()
                .filter_map(|id| ThreadId::from_string(id).ok())
                .collect::<Vec<_>>();
            self.forget_retired_agents(&ids).await;
            return;
        }
        if let ServerNotification::AccountLoginCompleted(completion) = &notification {
            self.finish_claude_login(completion);
        }
        if let ServerNotification::TurnModerationMetadata(metadata) = &mut notification
            && self
                .primary_thread_id
                .is_none_or(|id| id.to_string() != metadata.thread_id)
            && let Some(metadata) = metadata.metadata.as_object_mut()
        {
            metadata.remove("safety_block");
            metadata.remove("safety_recourse");
        }
        // A picker can leave an old runtime's close notification queued while the same thread
        // is resumed. Thread IDs survive reloads, so confirm that the displayed thread is still
        // unloaded before routing a close that would exit the TUI or switch away from it.
        if let ServerNotification::ThreadClosed(closed) = &notification
            && let Ok(thread_id) = ThreadId::from_string(&closed.thread_id)
            && self.current_displayed_thread_id() == Some(thread_id)
        {
            self.verify_thread_closure(app_server_client, thread_id, notification);
            return;
        }
        self.route_verified_server_notification(app_server_client, notification)
            .await;
    }

    pub(super) async fn route_verified_server_notification(
        &mut self,
        app_server_client: &AppServerSession,
        notification: ServerNotification,
    ) {
        if let ServerNotification::ThreadStarted(started) = &notification
            && started.thread.ephemeral
            && matches!(
                started.thread.thread_source.as_ref(),
                Some(ThreadSource::Feature(feature)) if matches!(feature.as_str(), "system" | "thread_title")
            )
        {
            return;
        }
        // Hidden helper threads must not enter visible thread routing or overview refreshes.
        let target = server_notification_thread_target(&notification);
        if let ServerNotificationThreadTarget::Thread(thread_id) = target
            && let Some(sender) = self.temporary_structured_requests.get(&thread_id)
        {
            if matches!(
                &notification,
                ServerNotification::ItemCompleted(_) | ServerNotification::TurnCompleted(_)
            ) && sender.send(notification).is_err()
            {
                self.temporary_structured_requests.remove(&thread_id);
            }

            return;
        }
        // Provisional lifecycle traffic belongs to the captured attachment before any global
        // visibility hook can project an obsolete runtime's close.
        if let ServerNotificationThreadTarget::Thread(thread_id) = target
            && self.saved_openings.observe(
                app_server_client.backend_id(),
                thread_id,
                self.thread_event_channels.get(&thread_id),
                &notification,
            )
        {
            return;
        }
        self.handle_agent_picker_visibility_notification(app_server_client, &notification);

        if let ServerNotification::ThreadStarted(started) = &notification
            && let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            }) = &started.thread.source
            && self
                .agents_overview
                .dispatched_requests
                .contains_key(parent_thread_id)
            && let Ok(thread_id) = codex_protocol::ThreadId::from_string(&started.thread.id)
        {
            self.agents_overview
                .dispatched_requests
                .entry(thread_id)
                .or_default();
        }
        self.track_subagent_panel(&notification);
        if let ServerNotification::ThreadStarted(started) = &notification
            && matches!(
                started.thread.source,
                SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
            )
            && let Ok(id) = ThreadId::from_string(&started.thread.id)
            && !self.is_family_thread(id)
            && let Some(root) = self.primary_thread_id
        {
            // A grandchild may arrive before its parent's metadata. The root-scoped list is
            // authoritative and bounded; do not cache foreign spawn trees to retry admission.
            if !self.agent_navigation.queue_picker_refresh(root) {
                self.refresh_agent_picker_threads(app_server_client, root);
            }
        }
        if matches!(
            &notification,
            ServerNotification::ThreadClosed(_) | ServerNotification::TurnCompleted(_)
        ) && let Some(root) = self.primary_thread_id
            && let super::app_server_event_targets::ServerNotificationThreadTarget::Thread(id) =
                super::app_server_event_targets::server_notification_thread_target(&notification)
            && (id == root || self.agent_navigation.get(&id).is_some())
        {
            self.refresh_agent_picker_threads(app_server_client, root);
        }
        self.track_agents_overview_notification(&notification);
        // Retained blank sessions stay subscribed after their event channels are cleared.
        if let ServerNotification::ThreadSettingsUpdated(settings) = &notification
            && let Ok(thread_id) = ThreadId::from_string(&settings.thread_id)
            && self.agents_overview.blank_sessions.contains_key(&thread_id)
            && !self.thread_event_channels.contains_key(&thread_id)
        {
            self.apply_thread_settings_to_cached_session(thread_id, &settings.thread_settings)
                .await;
            if let Some(input) = self.agents_overview.input_states.get_mut(&thread_id) {
                input.pending_thread_settings = Some(settings.clone());
            }
        }
        if matches!(
            &notification,
            ServerNotification::ThreadStarted(_)
                | ServerNotification::ThreadStatusChanged(_)
                | ServerNotification::ThreadSettingsUpdated(_)
                | ServerNotification::ThreadNameUpdated(_)
                | ServerNotification::ThreadArchived(_)
                | ServerNotification::ThreadDeleted(_)
                | ServerNotification::ThreadClosed(_)
        ) {
            self.repaint_agents_overview();
            self.refresh_changed_agents_overview_threads(app_server_client);
        }
        match &notification {
            ServerNotification::ServerRequestResolved(notification) => {
                let notification_thread_id =
                    codex_protocol::ThreadId::from_string(&notification.thread_id).ok();
                self.pending_primary_events.retain(|event| {
                    !matches!(event, ThreadBufferedEvent::Request(request)
                        if request.id() == &notification.request_id
                            && server_request_thread_id(request) == notification_thread_id)
                });
                if let Some(thread_id) = notification_thread_id
                    && let Some(requests) =
                        self.agents_overview.dispatched_requests.get_mut(&thread_id)
                {
                    requests.retain(|request| request.request.id() != &notification.request_id);
                }
                if let Some(resolved) = self
                    .pending_app_server_requests
                    .resolve_notification(&notification.thread_id, &notification.request_id)
                    && self.chat_widget.thread_id().map(|id| id.to_string())
                        == Some(resolved.thread_id)
                {
                    self.chat_widget
                        .dismiss_app_server_request(&resolved.request);
                    if self.startup_pending_protected_request {
                        self.startup_pending_protected_request =
                            self.chat_widget.has_active_modal()
                                || self.chat_widget.has_pending_protected_request();
                    }
                }
            }
            ServerNotification::McpServerOauthLoginCompleted(notification) => {
                // The start response identifies the new attempt. Hold completions until then
                // so a replacement's cancellation cannot appear as a fresh login failure.
                if let Some(pending) = self.pending_mcp_login_start.as_mut()
                    && pending.name == notification.name
                {
                    pending.completions.push(notification.clone());
                    return;
                }
                if notification.login_id.is_some() {
                    if notification.login_id.as_ref()
                        != self.active_mcp_login_ids.get(&notification.name)
                    {
                        return;
                    }
                    self.active_mcp_login_ids.remove(&notification.name);
                }
            }

            ServerNotification::AccountRateLimitsUpdated(notification) => {
                if notification.thread_id.as_deref().is_some_and(|id| {
                    self.current_displayed_thread_id()
                        .is_none_or(|current| current.to_string() != id)
                }) {
                    return;
                }
                let workspace_hard_stop = matches!(
                    notification.rate_limits.rate_limit_reached_type,
                    Some(
                        RateLimitReachedType::WorkspaceOwnerCreditsDepleted
                            | RateLimitReachedType::WorkspaceMemberCreditsDepleted
                            | RateLimitReachedType::WorkspaceOwnerUsageLimitReached
                            | RateLimitReachedType::WorkspaceMemberUsageLimitReached
                    )
                ) || notification.rate_limits.spend_control_reached
                    == Some(true);
                if workspace_hard_stop {
                    self.rate_limit_hard_stop_generation =
                        self.rate_limit_hard_stop_generation.wrapping_add(1);
                }
                self.chat_widget
                    .on_rolling_rate_limit_snapshot(notification.rate_limits.clone());
                if workspace_hard_stop && self.chat_widget.has_chatgpt_account() {
                    // Background inference may publish a hard stop without a foreground Error.
                    self.refresh_rate_limits(app_server_client, RateLimitRefreshOrigin::Recovery);
                }
                return;
            }
            ServerNotification::AccountUpdated(notification) => {
                self.chat_widget.update_backend_authority(
                    crate::app_server_session::BackendAuthority::Reported(notification.auth_mode),
                );
                self.rate_limit_hard_stop_generation =
                    self.rate_limit_hard_stop_generation.wrapping_add(1);
                self.rate_limit_refresh_state.invalidate_recovery();
                self.agents_overview.usage.clear();
                self.agents_overview.pending_usage = None;
                self.agents_overview.usage_disabled = false;
                self.repaint_agents_overview();
                if self.chat_widget.account_ready_reason().is_some() {
                    if let Some(thread_id) = self.current_displayed_thread_id() {
                        self.refresh_conversation_account(app_server_client, thread_id);
                    }
                    return;
                }
                // Global-login notifications cannot replace a draft's fresh-root identity.
                if self.chat_widget.session_draft.is_some() {
                    return;
                }
                if let Some(thread_id) = self.current_displayed_thread_id()
                    && !self
                        .account_ui
                        .displayed
                        .as_ref()
                        .is_some_and(|(id, account)| {
                            *id == Some(thread_id) && account.account_id.is_none()
                        })
                {
                    self.refresh_conversation_account(app_server_client, thread_id);
                    return;
                }
                self.chat_widget.invalidate_security_setup();
                if let Some(crate::pager_overlay::Overlay::Analytics(view)) = &mut self.overlay {
                    view.refresh();
                }
                if let Some(view) = &mut self.retained_analytics {
                    view.cancel_loads();
                }
                // Deferred terminal writes must never carry the previous account's billing into
                // the newly authenticated identity, even when both accounts share one thread.
                self.last_thread_usage_status_cell = None;
                self.pending_thread_usage_history_refresh = false;
                self.read_displayed_account(
                    app_server_client,
                    self.current_displayed_thread_id().map(|id| id.to_string()),
                );
                return;
            }
            ServerNotification::AppListUpdated(notification) => {
                if self.current_displayed_thread_id().is_some() {
                    self.chat_widget
                        .refresh_connector_directory_after_notification(
                            notification
                                .data
                                .iter()
                                .cloned()
                                .map(app_info_from_api)
                                .collect(),
                        );
                }
                return;
            }
            _ => {}
        }

        match target {
            ServerNotificationThreadTarget::Thread(thread_id) => {
                let attaching_saved = self
                    .saved_openings
                    .preparing(thread_id, self.thread_event_channels.get(&thread_id));
                if self.current_displayed_thread_id() != Some(thread_id)
                    && let ServerNotification::ItemCompleted(item) = &notification
                    && let ThreadItemPayload::UserMessage {
                        client_id: Some(client_id),
                        ..
                    } = &item.item.payload
                {
                    // Acknowledge by ID before routing can discard the receipt. ID-less receipts
                    // cannot safely distinguish identical pending submissions.
                    let mut store = match self.thread_event_channels.get(&thread_id) {
                        Some(channel) => Some(channel.store.lock().await),
                        None => None,
                    };
                    for input in store
                        .as_mut()
                        .and_then(|store| store.input_state.as_mut())
                        .into_iter()
                        .chain(self.agents_overview.input_states.get_mut(&thread_id))
                    {
                        input.input_queue.discard_pending_steer(client_id);
                    }
                }
                let background_voice = self.background_voice.as_ref().is_some_and(|owner| {
                    owner.thread_id() == Some(thread_id) && owner.realtime_conversation_is_running()
                });
                if self.primary_thread_id.is_none()
                    && !self.pending_startup_thread_start
                    && !background_voice
                    && !attaching_saved
                {
                    return;
                }
                let untracked_thread =
                    self.primary_thread_id.is_some() && !self.owns_thread_for_routing(thread_id);
                if untracked_thread && !background_voice && !attaching_saved {
                    if matches!(notification, ServerNotification::McpServerStatusUpdated(_)) {
                        self.verify_untracked_mcp_notification(
                            app_server_client,
                            thread_id,
                            notification,
                        );
                        return;
                    }
                    if !self.owns_untracked_notification(&notification) {
                        return;
                    }
                }
                let result = if self.primary_thread_id.is_none() && !background_voice {
                    self.enqueue_primary_thread_notification(notification).await
                } else {
                    self.enqueue_thread_notification(thread_id, notification)
                        .await
                };

                if let Err(err) = result {
                    tracing::warn!("failed to enqueue app-server notification: {err}");
                }
                return;
            }
            ServerNotificationThreadTarget::InvalidThreadId(thread_id) => {
                tracing::warn!(
                    thread_id,
                    "ignoring app-server notification with invalid thread_id"
                );
                return;
            }
            ServerNotificationThreadTarget::AppScoped => {
                tracing::debug!(
                    "ignoring app-scoped MCP startup notification without a TUI app-level target"
                );
                return;
            }
            ServerNotificationThreadTarget::Global => {}
        }

        #[cfg(any(target_os = "windows", test))]
        if let ServerNotification::WindowsSandboxSetupCompleted(result) = notification {
            let Some((mode, preset, profile_selection)) =
                self.windows_sandbox.pending_setup.clone()
            else {
                return;
            };
            let expected_mode = match mode {
                WindowsSandboxEnableMode::Elevated => {
                    codex_app_server_protocol::WindowsSandboxSetupMode::Elevated
                }
                WindowsSandboxEnableMode::Legacy => {
                    codex_app_server_protocol::WindowsSandboxSetupMode::Unelevated
                }
            };
            if result.mode != expected_mode {
                return;
            }
            if result.success {
                self.app_event_tx
                    .send(AppEvent::EnableWindowsSandboxForAgentMode {
                        preset,
                        mode,
                        profile_selection,
                    });
            } else if mode == WindowsSandboxEnableMode::Elevated {
                self.app_event_tx
                    .send(AppEvent::OpenWindowsSandboxFallbackPrompt {
                        preset,
                        profile_selection,
                    });
            } else {
                self.windows_sandbox.pending_setup = None;
                self.chat_widget.clear_windows_sandbox_setup_status();
                self.windows_sandbox.setup_started_at = None;
                if self
                    .chat_widget
                    .recover_inline_permission_profile_selection()
                {
                    self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
                } else {
                    self.chat_widget
                        .retain_input_after_failed_permission_selection();
                }
                self.chat_widget.add_error_message(format!(
                    "Windows sandbox setup failed: {}",
                    result.error.unwrap_or_else(|| "unknown error".to_string())
                ));
            }
            return;
        }

        self.chat_widget
            .handle_server_notification(notification, /*replay_kind*/ None);
    }

    pub(in crate::app) async fn handle_server_request_event(
        &mut self,
        app_server_client: &AppServerSession,
        buffered_request: BufferedServerRequest,
    ) {
        let request = &buffered_request.request;
        let thread_id = server_request_thread_id(request);
        let attaching_saved = thread_id.is_some_and(|id| {
            self.saved_openings
                .preparing(id, self.thread_event_channels.get(&id))
        });
        if thread_id
            .is_some_and(|thread_id| self.temporary_structured_requests.contains_key(&thread_id))
        {
            self.reject_unsupported_app_server_request(
                app_server_client,
                super::app_server_requests::UnsupportedAppServerRequest {
                    request_id: request.id().clone(),
                    message:
                        "temporary structured threads cannot request tools or user interaction"
                            .to_string(),
                },
                &buffered_request,
            );
            return;
        }
        if let ServerRequest::DynamicToolCall { request_id, .. } = &request {
            let response = crate::dynamic_tools::failure_response(
                "Thread coordination is owned by the app server; this frontend does not execute task tools.",
            );
            match serde_json::to_value(response) {
                Ok(result) => self.enqueue_connection_operation(
                    app_server_client,
                    super::turn_submissions::ServerOperation::Response(Box::new(
                        super::turn_submissions::ServerRequestDelivery::Resolved {
                            reply: buffered_request.reply,
                            result,
                        },
                    )),
                ),
                Err(error) => {
                    tracing::warn!(?request_id, %error, "failed to serialize dynamic tool response")
                }
            }
            return;
        }

        let background_voice = self.background_voice.as_ref().is_some_and(|owner| {
            owner.realtime_conversation_is_running()
                && owner.thread_id().is_some()
                && owner.thread_id() == thread_id
        });
        if thread_id.is_some_and(|thread_id| self.abandoned_side_threads.contains(&thread_id)) {
            self.reject_unsupported_app_server_request(
                app_server_client,
                super::app_server_requests::UnsupportedAppServerRequest {
                    request_id: request.id().clone(),
                    message: "side conversation was closed".to_string(),
                },
                &buffered_request,
            );
            return;
        }
        if thread_id.is_some()
            && self.primary_thread_id.is_none()
            && !background_voice
            && self.pending_startup_thread_start
            && !attaching_saved
        {
            if let Some(unsupported) = self
                .pending_app_server_requests
                .note_buffered_request(&buffered_request)
            {
                self.reject_unsupported_app_server_request(
                    app_server_client,
                    unsupported,
                    &buffered_request,
                );
                return;
            }
            self.pending_primary_events
                .push_back(ThreadBufferedEvent::Request(Box::new(
                    buffered_request.request,
                )));
            return;
        }
        let unsupported_request = matches!(
            &request,
            ServerRequest::DynamicToolCall { .. }
                | ServerRequest::AttestationGenerate { .. }
                | ServerRequest::CurrentTimeRead { .. }
                | ServerRequest::ApplyPatchApproval { .. }
                | ServerRequest::ExecCommandApproval { .. }
        );
        if let ServerRequest::PermissionsRequestApproval { request_id, params } = &request
            && let Err(err) = CoreRequestPermissionProfile::try_from(params.permissions.clone())
        {
            self.reject_unsupported_app_server_request(
                app_server_client,
                super::app_server_requests::UnsupportedAppServerRequest {
                    request_id: request_id.clone(),
                    message: format!("failed to localize requested filesystem paths: {err}"),
                },
                &buffered_request,
            );
            return;
        }
        if self
            .pending_app_server_requests
            .contains_server_request(request)
        {
            return;
        }
        if let Some(thread_id) = thread_id
            && self.primary_thread_id != Some(thread_id)
            && self.active_thread_id != Some(thread_id)
            && !unsupported_request
            && !background_voice
            && !attaching_saved
            && let Some(requests) = self.agents_overview.dispatched_requests.get_mut(&thread_id)
        {
            requests.push(buffered_request);
            return;
        }
        if thread_id.is_some()
            && self.primary_thread_id.is_none()
            && !background_voice
            && !self.pending_startup_thread_start
            && !unsupported_request
            && !attaching_saved
        {
            return;
        }
        if let Some(thread_id) = thread_id
            && self.primary_thread_id.is_some()
            && !self.is_attached_thread(thread_id)
            && !background_voice
            && !unsupported_request
            && !attaching_saved
        {
            let thread = app_server_client
                .request_handle()
                .request_typed::<ThreadReadResponse>(ClientRequest::ThreadRead {
                    request_id: RequestId::String(format!("subagent-approval-{thread_id}")),
                    params: ThreadReadParams {
                        thread_id: thread_id.to_string(),
                        include_turns: false,
                    },
                })
                .await;
            let Ok(ThreadReadResponse { thread, .. }) = thread else {
                return;
            };
            let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            }) = &thread.source
            else {
                return;
            };
            if !self.is_family_thread(*parent_thread_id) {
                if self
                    .agents_overview
                    .dispatched_requests
                    .contains_key(parent_thread_id)
                {
                    self.agents_overview
                        .dispatched_requests
                        .entry(thread_id)
                        .or_default()
                        .push(buffered_request);
                    return;
                }
                let Some(root) = self.primary_thread_id else {
                    return;
                };
                let descendants = super::agent_picker::list_agent_picker_threads(
                    &app_server_client.request_handle(),
                    root,
                )
                .await;
                match descendants {
                    Ok(refresh)
                        if refresh
                            .threads
                            .iter()
                            .any(|candidate| candidate.id == thread.id)
                            || refresh.archived_thread_ids.contains(&thread_id) =>
                    {
                        self.agent_navigation
                            .descendant_roots
                            .insert(thread_id, root);
                    }
                    Ok(_) => return,
                    Err(err) => {
                        tracing::warn!(%err, %thread_id, "could not verify pending request ancestry");
                        return;
                    }
                }
            }
            self.hydrate_started_thread(&thread);
        }

        let unsupported = self
            .pending_app_server_requests
            .note_buffered_request(&buffered_request);
        if let Some(unsupported) = unsupported {
            self.reject_unsupported_app_server_request(
                app_server_client,
                unsupported,
                &buffered_request,
            );
            return;
        }

        let Some(thread_id) = thread_id else {
            tracing::warn!("ignoring threadless app-server request");
            return;
        };

        let result = if self.primary_thread_id.is_none() && !background_voice && !attaching_saved {
            self.enqueue_primary_thread_request(buffered_request.request)
                .await
        } else {
            self.enqueue_thread_request(thread_id, buffered_request.request)
                .await
        };
        if let Err(err) = result {
            tracing::warn!("failed to enqueue app-server request: {err}");
        }
    }
}
