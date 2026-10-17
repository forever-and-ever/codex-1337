//! Directory transitions preserve conversation history and revalidate destination permissions.

mod admission;

use super::session_lifecycle::ThreadAttachPresentation;
use super::*;
use crate::history_cell::McpInventoryLoadingCell as LoadingCell;
use codex_app_server_protocol::ThreadBackgroundTerminalsListResponse as ListResponse;

/// Validated destination session and its matching frontend configuration.
pub(super) struct DirectoryAttachment {
    pub(super) started: AppServerStartedThread,
    pub(super) config: Box<Config>,
    pub(super) local_settings: crate::local_settings::LocalSettings,
    pub(super) keymap: RuntimeKeymap,
    pub(super) cwd: AbsolutePathBuf,
    pub(super) selected_profile: Option<PermissionProfileSelection>,
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
        _tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        cwd: AbsolutePathBuf,
    ) {
        let std::ops::ControlFlow::Continue(cwd) = self.admit_directory_change(app_server, cwd)
        else {
            return;
        };
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
        let has_rollout = rollout.as_deref().is_some_and(rollout_path_is_resumable);
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
        let tracked_ids: Vec<_> = std::iter::once(thread_id)
            .chain(ids.iter().copied().filter(|id| *id != thread_id))
            .collect();

        self.enqueue_directory_change(
            app_server,
            thread_id,
            cwd,
            selected_profile,
            if has_rollout {
                super::turn_submissions::DirectoryHistory::Fork
            } else {
                super::turn_submissions::DirectoryHistory::Blank
            },
            tracked_ids,
        )
        .await;
    }

    #[cfg(test)]
    pub(super) async fn change_working_directory_and_settle(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        cwd: AbsolutePathBuf,
    ) {
        self.settle_turn_submissions(server)
            .await
            .expect("prior settings settle");
        self.change_working_directory(tui, server, cwd).await;
        self.settle_navigation(tui, server)
            .await
            .expect("directory navigation settles");
    }

    pub(super) async fn attach_working_directory(
        &mut self,
        tui: &mut tui::Tui,
        _app_server: &mut AppServerSession,
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
        tui.frame_requester().schedule_frame();
    }
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
