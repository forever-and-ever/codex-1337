//! Directory transitions preserve conversation history and revalidate destination permissions.

use super::session_lifecycle::ThreadAttachPresentation;
use super::*;
use crate::app_server_session::ForkGoalContinuation::DeferUntilNextTurn;
use crate::history_cell::McpInventoryLoadingCell as LoadingCell;
use codex_app_server_protocol::ThreadBackgroundTerminalsListParams;
use codex_app_server_protocol::ThreadBackgroundTerminalsListResponse as ListResponse;

/// Validated destination session and its matching frontend configuration.
pub(super) struct DirectoryAttachment {
    started: AppServerStartedThread,
    config: Box<Config>,
    local_settings: crate::local_settings::LocalSettings,
    keymap: RuntimeKeymap,
    cwd: AbsolutePathBuf,
    selected_profile: Option<PermissionProfileSelection>,
}

/// A /cd request awaiting a fresh event-loop iteration.
pub(super) struct PendingWorkingDirectoryChange {
    pub(super) source_thread_id: ThreadId,
    pub(super) source_cwd: AbsolutePathBuf,
    pub(super) destination: AbsolutePathBuf,
}

impl App {
    pub(super) async fn finish_working_directory_change(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        pending: PendingWorkingDirectoryChange,
    ) {
        if self.primary_thread_id != Some(pending.source_thread_id)
            || self.config.cwd != pending.source_cwd
            || !self
                .chat_widget
                .can_change_working_directory(pending.source_thread_id)
        {
            return self.working_directory_error(
                "Changing directories requires an idle primary session without queued input.",
            );
        }
        if crate::uses_remote_workspace_or_environment(
            &self.app_server_target,
            self.environment_manager.as_ref(),
        ) {
            return self.working_directory_error(
                "Changing directories is not supported for remote workspaces or remote execution environments.",
            );
        }
        self.change_working_directory(tui, app_server, pending.destination)
            .await;
    }

    pub(super) fn working_directory_error(&mut self, message: impl Into<String>) {
        self.chat_widget.add_error_message(message.into());
    }

    pub(super) async fn change_working_directory(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        cwd: AbsolutePathBuf,
    ) {
        if self.config.ephemeral || !cwd.as_path().is_dir() {
            return self.working_directory_error("This task cannot be safely replaced.");
        }
        let Some(thread_id) = self.chat_widget.thread_id() else {
            return;
        };
        if self.pending_server_profiles.contains_key(&thread_id) {
            return self.working_directory_error(
                "Wait for permissions to update before changing directories.",
            );
        }
        if self.app_server_target.thread_params_mode()
            == crate::app_server_session::ThreadParamsMode::Remote
            && self
                .chat_widget
                .config_ref()
                .permissions
                .active_permission_profile()
                .is_some_and(|profile| !profile.id.starts_with(':'))
        {
            return self.working_directory_error(
                "Changing directories with a named profile is not supported.",
            );
        }
        let selected_profile = self
            .agents_overview
            .requested_permission_profiles
            .get(&thread_id)
            .cloned();
        if selected_profile
            .as_ref()
            .is_some_and(|profile| !profile.profile_id.starts_with(':'))
        {
            return self.working_directory_error(
                "Changing directories with an unconfirmed named profile is not supported.",
            );
        }
        let cells = &self.transcript_cells;
        if cells.iter().any(|cell| cell.as_any().is::<LoadingCell>()) {
            return self.working_directory_error("MCP inventory is still loading.");
        }
        let agents = self.agent_navigation.ordered_threads();
        let closed_agents: HashSet<_> = agents
            .iter()
            .filter_map(|(id, agent)| agent.is_closed.then_some(*id))
            .collect();
        let active = self.thread_event_channels.iter().any(|(id, channel)| {
            *id != thread_id
                && !closed_agents.contains(id)
                && !channel
                    .store
                    .try_lock()
                    .is_ok_and(|store| store.active_turn_id().is_none())
        });
        if active || agents.iter().any(|(t, a)| *t != thread_id && a.is_running) {
            return self.working_directory_error("Cannot change: another agent is running.");
        }
        let open_agents: Vec<_> = agents
            .iter()
            .filter_map(|(id, agent)| (!agent.is_closed).then_some(*id))
            .collect();
        let rollout = self.chat_widget.rollout_path();
        let mut has_rollout = rollout.as_deref().is_some_and(rollout_path_is_resumable);
        let channels = &self.thread_event_channels;
        if !has_rollout
            && (!self.chat_widget.token_usage().is_zero()
                || channels.get(&thread_id).is_some_and(|channel| {
                    channel.store.try_lock().map_or(/*default*/ true, |store| {
                        !store.turns.is_empty()
                            || store.buffer.iter().any(|event| {
                                matches!(
                                    event,
                                    ThreadBufferedEvent::Notification(notification)
                                        if matches!(notification.as_ref(),
                                            ServerNotification::TurnStarted(_)
                                            | ServerNotification::TurnCompleted(_))
                                )
                            })
                    })
                }))
        {
            return self.working_directory_error("Conversation history is not saved.");
        }
        let mut ids: HashSet<_> = channels
            .keys()
            .copied()
            .filter(|id| !closed_agents.contains(id))
            .collect();
        ids.extend(open_agents);
        let mut tracked_ids: Vec<_> = std::iter::once(thread_id)
            .chain(ids.iter().copied().filter(|id| *id != thread_id))
            .collect();
        let mut config = match self.rebuild_config_for_cwd(cwd.to_path_buf()).await {
            Ok(config) => config,
            Err(err) => return self.working_directory_error(format!("Cannot load {cwd:?}: {err}")),
        };
        if config.active_project.trust_level.is_none() {
            if let Some(message) = check_background_terminals(
                app_server,
                &self.app_server_target,
                tracked_ids.iter().copied(),
            )
            .await
            {
                return self.working_directory_error(message);
            }
            if self
                .confirm_directory_trust(
                    tui,
                    app_server,
                    &mut config,
                    cwd.as_path(),
                    crate::onboarding::DirectoryTrustOptions {
                        cancel: Some(crate::onboarding::TrustCancelAction::CurrentTask),
                        ..Default::default()
                    },
                    /*startup_draft*/ None,
                )
                .await
                .is_err()
            {
                return;
            }
        }
        if !has_rollout
            || (self.runtime_permission_profile_override.is_none()
                && self
                    .config
                    .permissions
                    .active_permission_profile()
                    .is_some_and(|profile| {
                        profile.id == codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE
                    }))
        {
            match crate::config_update::read_effective_config_if_supported(
                app_server.request_handle(),
                cwd.as_path(),
            )
            .await
            {
                Ok(Some(defaults)) => {
                    crate::projectless::apply_defaults(
                        &mut config,
                        &self.harness_overrides,
                        app_server,
                        &self.environment_manager,
                        &defaults,
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    return self
                        .working_directory_error(format!("Cannot read defaults: {error:#}"));
                }
            }
        }
        let scope = if config.active_project.is_untrusted() {
            RuntimePolicyOverrideScope::ExplicitOnly
        } else {
            RuntimePolicyOverrideScope::All
        };
        let profile = self
            .runtime_permission_profile_override
            .as_ref()
            .filter(|_| selected_profile.is_none())
            .filter(|profile| {
                scope == RuntimePolicyOverrideScope::All
                    || profile.turn_override == RuntimePermissionProfileTurnOverride::LegacySandbox
            })
            .cloned();
        if let Some(profile) = profile.as_ref()
            && profile
                .active_permission_profile
                .as_ref()
                .is_some_and(|active| !active.id.starts_with(':'))
            && (!profile.matches_config(&config)
                || config.permissions.profile_workspace_roots()
                    != self.config.permissions.profile_workspace_roots())
        {
            return self.working_directory_error("Permission profile has different settings.");
        }
        if let Some(profile) = profile.as_ref()
            && profile.turn_override == RuntimePermissionProfileTurnOverride::Preserve
            && profile.active_permission_profile.is_none()
            && !crate::app_server_session::permission_profile_is_safely_represented_by_sandbox_mode(
                &profile.permission_profile,
                cwd.as_path(),
            )
        {
            return self.working_directory_error("Permission profile cannot be preserved by /cd.");
        }
        if selected_profile.is_none()
            && let Err(error) = self.apply_runtime_policy_overrides(&mut config, scope)
        {
            return self.working_directory_error(format!("{error:#}"));
        }
        let actual = config.permissions.approval_policy.value();
        let approval = self
            .runtime_approval_policy_override
            .filter(|_| selected_profile.is_none())
            .filter(|value| {
                scope == RuntimePolicyOverrideScope::All
                    || matches!(value, RuntimeApprovalPolicyOverride::Explicit(_))
            })
            .map(RuntimeApprovalPolicyOverride::policy);
        if approval.is_some_and(|p| actual != p.to_core())
            || profile.is_some_and(|profile| !profile.matches_config(&config))
        {
            return;
        }
        let local_settings = self.local_settings.reloaded(&config);
        let keymap = match RuntimeKeymap::from_config(&local_settings.tui.keymap) {
            Ok(keymap) => keymap,
            Err(error) => return self.chat_widget.add_error_message(error),
        };
        config.service_tier = self.chat_widget.configured_service_tier();
        // Config reads and folder consent can overlap work from another client.
        if !self
            .backfill_loaded_subagent_threads(app_server)
            .await
            .completed
        {
            return self.working_directory_error("Cannot check agent status.");
        }
        ids.extend(
            self.agent_navigation
                .ordered_threads()
                .iter()
                .filter_map(|(id, agent)| (!agent.is_closed).then_some(*id)),
        );
        tracked_ids = std::iter::once(thread_id)
            .chain(ids.iter().copied().filter(|id| *id != thread_id))
            .collect();
        for id in &tracked_ids {
            match app_server.thread_read(*id, /*include_turns*/ false).await {
                Ok(thread)
                    if matches!(
                        thread.status,
                        codex_app_server_protocol::ThreadStatus::Active { .. }
                    ) =>
                {
                    return self.working_directory_error("Cannot change: a task is running.");
                }
                Ok(_) => {}
                Err(error) => {
                    return self
                        .working_directory_error(format!("Cannot check task status: {error}"));
                }
            }
        }
        if let Some(message) =
            check_background_terminals(app_server, &self.app_server_target, tracked_ids).await
        {
            return self.working_directory_error(message);
        }
        has_rollout |= rollout.as_deref().is_some_and(rollout_path_is_resumable);
        let preserve_history = has_rollout;
        let transitioned = if preserve_history {
            app_server
                .fork_thread_at(
                    &local_settings,
                    config.clone(),
                    thread_id,
                    /*last_turn_id*/ None,
                    /*before_turn_id*/ None,
                    DeferUntilNextTurn,
                    selected_profile.as_ref(),
                    /*name*/ None,
                )
                .await
        } else {
            app_server
                .start_thread_with_session_start_source(
                    &local_settings,
                    &config,
                    /*session_start_source*/ None,
                    /*remote_cwd_override*/ None,
                    selected_profile.as_ref(),
                )
                .await
        };
        let transitioned = match transitioned {
            Ok(value) => value,
            Err(e) => return self.working_directory_error(format!("Failed to change: {e}")),
        };
        let session = &transitioned.session;
        let permissions_match = if let Some(selected) = &selected_profile {
            session
                .active_permission_profile
                .as_ref()
                .is_some_and(|active| active.id == selected.profile_id)
                && selected
                    .approval_policy
                    .is_none_or(|policy| session.approval_policy == policy)
                && selected
                    .approvals_reviewer
                    .is_none_or(|reviewer| session.approvals_reviewer == reviewer)
        } else {
            session.approval_policy.to_core() == config.permissions.approval_policy.value()
                && session.approvals_reviewer == config.approvals_reviewer
                && session.active_permission_profile
                    == config.permissions.active_permission_profile()
        };
        if session.thread_id == thread_id
            || crate::session_resume::cwds_differ(session.cwd.as_path(), cwd.as_path())
            || session.runtime_workspace_roots != config.workspace_roots
            || !permissions_match
        {
            if session.thread_id != thread_id {
                let _ = app_server.thread_unsubscribe(session.thread_id).await;
                if preserve_history {
                    let _ = app_server.thread_archive(session.thread_id).await;
                }
            }
            return self.working_directory_error("Requested directory or permissions not applied.");
        }
        if let Err(error) = app_server.thread_unsubscribe(thread_id).await {
            let replacement_id = transitioned.session.thread_id;
            let _ = app_server.thread_unsubscribe(replacement_id).await;
            if preserve_history {
                let _ = app_server.thread_archive(replacement_id).await;
            }
            return self.working_directory_error(format!("Cannot change directories: {error}"));
        }
        for tracked_id in ids.into_iter().filter(|id| *id != thread_id) {
            if let Err(error) = app_server.thread_unsubscribe(tracked_id).await {
                tracing::warn!("failed to unsubscribe tracked thread {tracked_id}: {error}");
            }
        }
        let attach = DirectoryAttachment {
            started: transitioned,
            config: Box::new(config),
            local_settings,
            keymap,
            cwd,
            selected_profile,
        };
        self.attach_working_directory(tui, app_server, attach).await;
    }

    pub(super) async fn attach_working_directory(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        attach: DirectoryAttachment,
    ) {
        let DirectoryAttachment {
            started,
            config,
            local_settings,
            keymap,
            cwd,
            selected_profile,
        } = attach;
        self.local_settings = local_settings;
        self.refresh_server_version_overview_notice(CODEX_CLI_VERSION);
        self.config = *config;
        self.file_search
            .update_search_dir(self.config.cwd.to_path_buf());
        let notify = &self.local_settings.tui.notification_settings;
        tui.set_notification_settings(notify);
        if let Err(error) = tui.clear_ambient_pet_image() {
            tracing::warn!(%error, "failed to clear ambient pet image");
        }
        let attach_widget = App::replace_chat_widget_with_app_server_thread;
        let (lineage, message) = (ThreadAttachPresentation::SessionLineage, None);
        if let Err(error) = attach_widget(self, tui, started, lineage, message).await {
            return self.working_directory_error(format!("Could not restore session: {error}"));
        }
        if selected_profile.is_some() {
            self.adopt_inherited_server_selection();
        }
        if matches!(
            self.runtime_approval_policy_override,
            Some(RuntimeApprovalPolicyOverride::Restored(_))
        ) {
            self.runtime_approval_policy_override = Some(RuntimeApprovalPolicyOverride::Restored(
                self.config.permissions.approval_policy.value().into(),
            ));
        }
        if self
            .runtime_permission_profile_override
            .as_ref()
            .is_some_and(|profile| {
                profile.turn_override == RuntimePermissionProfileTurnOverride::Preserve
            })
        {
            self.runtime_permission_profile_override = Some(
                RuntimePermissionProfileOverride::from_restored_config(&self.config),
            );
        }
        self.cancel_pending_key_chord();
        self.keymap = keymap;
        self.merge_startup_warnings(tui, &history_cell::StartupWarningsCell::default());
        self.restore_runtime_theme_from_config();
        self.runtime_working_directory_override = Some(cwd.to_path_buf());
        if let Some(message) = project_config_warning(&self.config) {
            self.chat_widget.add_warning_message(message);
        }
        let message = format!("Working directory changed to: {}", cwd.display());
        self.chat_widget.add_info_message(message, /*hint*/ None);
        if !self.config.bypass_hook_trust {
            let load_review = crate::startup_hooks_review::load_startup_hooks_review_entry;
            let hooks = load_review(app_server.request_handle(), cwd.to_path_buf()).await;
            if hooks.hooks.iter().any(crate::hooks_rpc::hook_needs_review) {
                self.chat_widget.open_hooks_browser(hooks);
            }
        }
        tui.frame_requester().schedule_frame();
    }
}

pub(super) async fn check_background_terminals(
    app_server: &mut AppServerSession,
    target: &AppServerTarget,
    thread_ids: impl IntoIterator<Item = ThreadId>,
) -> Option<&'static str> {
    for thread_id in thread_ids {
        let request = ClientRequest::ThreadBackgroundTerminalsList {
            request_id: app_server.next_request_id(),
            params: ThreadBackgroundTerminalsListParams {
                thread_id: thread_id.to_string(),
                cursor: None,
                limit: Some(1),
            },
        };
        let result = app_server
            .request_handle()
            .request_typed::<ListResponse>(request)
            .await;
        if let Some(message) = background_terminals_blocker(result, target) {
            return Some(message);
        }
    }
    None
}

pub(super) fn background_terminals_blocker(
    result: Result<ListResponse, TypedRequestError>,
    target: &AppServerTarget,
) -> Option<&'static str> {
    match result {
        Ok(response) if response.data.is_empty() => None,
        Err(TypedRequestError::Server { source, .. })
            if matches!(target, AppServerTarget::LocalDaemon { .. })
                && (source.code == -32601
                    || source.code == -32600
                        && source.message.contains("thread/backgroundTerminals/list")
                        && (source.message.contains("unknown variant")
                            || source.message.contains("unknown method"))) =>
        {
            Some(
                "The local Codex service cannot check background terminals. Run `codex app-server daemon update`, then restart Codex.",
            )
        }
        _ => Some("Active background terminals block /cd."),
    }
}
