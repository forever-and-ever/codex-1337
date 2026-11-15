use super::App;
use super::AppRunControl;
use super::AppServerSession;
use super::AskForApproval;
use super::CODEX_CLI_VERSION;
use super::CleanupContinuation;
use super::NavigationIntent;
use super::PendingNavigation;
use super::PermissionProfileSnapshot;
use super::Result;
use super::ResumeEffect;
use super::ResumePresentation;
use super::ResumePurpose;
use super::ResumeReady;
use super::ResumeSource;
use super::ResumeStage;
use super::RuntimeApprovalPolicyOverride;
use super::RuntimePermissionProfileOverride;
use super::RuntimePermissionProfileTurnOverride;
use super::RuntimePolicyOverrideScope;
use super::SelectionAfter;
use super::SessionTarget;
use super::ThreadAttachPresentation;
use super::ThreadId;
use super::ThreadInputStateRestoreMode;
use super::api;
use super::tui;

impl App {
    pub(in crate::app) async fn select_agents_overview_thread_after(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        after: SelectionAfter,
    ) -> Result<AppRunControl> {
        if self.current_displayed_thread_id() == Some(thread_id)
            && self.thread_unavailable(thread_id)
            && !self.chat_widget.is_external_writer_view()
            && matches!(after, SelectionAfter::SelectOnly)
        {
            return Box::pin(
                self.resume_target_session(
                    tui,
                    server,
                    SessionTarget {
                        backend_id: server.backend_id(),
                        path: self
                            .primary_session_configured
                            .as_ref()
                            .and_then(|session| session.rollout_path.clone()),
                        thread_id,
                        cwd: None,
                        history_mode: None,
                    },
                ),
            )
            .await;
        }
        if self.windows_sandbox_blocks_thread_switch() {
            return Ok(AppRunControl::Continue);
        }
        if self.current_displayed_thread_id() == Some(thread_id)
            && (!self.thread_unavailable(thread_id) || self.chat_widget.is_external_writer_view())
        {
            self.supersede_pending_navigation();
            if let Ok(mut state) = self.agents_overview.view_state.lock() {
                state.completion = Some(crate::bottom_pane::ViewCompletion::Accepted);
            }
            self.finish_selection_after(tui, server, after).await?;
            self.drive_server_operations(server);
            self.refresh_submission_preview();
            self.chat_widget.pre_draw_tick();
            return Ok(AppRunControl::Continue);
        }
        if self.reject_pending_permission_root_switch() {
            return Ok(AppRunControl::Continue);
        }
        self.enqueue_overview_attachment(server, thread_id, after);
        Ok(AppRunControl::Continue)
    }

    pub(in crate::app) fn enqueue_overview_attachment(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        after: SelectionAfter,
    ) {
        self.enqueue_navigation(
            server,
            NavigationIntent::Resume(ResumeStage::OverviewAdmitted { thread_id, after }),
        );
    }

    #[cfg(test)]
    pub(in crate::app) async fn select_agents_overview_thread_and_settle(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<AppRunControl> {
        let control = self
            .select_agents_overview_thread(tui, server, thread_id)
            .await?;
        if matches!(control, AppRunControl::Continue) {
            return Box::pin(self.settle_navigation(tui, server)).await;
        }
        Ok(control)
    }

    pub(super) async fn begin_overview_attachment(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        operation: PendingNavigation,
        thread_id: ThreadId,
        thread: api::Thread,
        after: SelectionAfter,
    ) -> Result<AppRunControl> {
        if thread.id != thread_id.to_string() {
            self.add_agents_overview_error(
                "The task read returned a different conversation; no attachment was attempted."
                    .into(),
            );
            return Ok(AppRunControl::Continue);
        }
        let target = SessionTarget {
            backend_id: server.backend_id(),
            path: thread.path.clone(),
            thread_id,
            cwd: Some(thread.cwd.to_path_buf()),
            history_mode: Some(thread.history_mode),
        };
        if matches!(thread.status, api::ThreadStatus::NotLoaded) {
            let basis = match self.capture_resume_cwd_basis(server) {
                Ok(basis) => basis,
                Err(control) => return Ok(control),
            };
            let history_cwd = Some(thread.cwd.to_path_buf());
            return self
                .finish_resume_cwd(
                    tui,
                    server,
                    operation,
                    target,
                    basis,
                    history_cwd,
                    ResumeSource::Overview {
                        thread: Box::new(thread),
                        after,
                    },
                )
                .await;
        }
        let current_cwd = self.config.cwd.to_path_buf();
        let config_cwd = if self.app_server_target.uses_remote_workspace() {
            current_cwd.clone()
        } else {
            thread.cwd.to_path_buf()
        };
        let plan = self.capture_config_rebuild(config_cwd, Some(&current_cwd));
        self.start_resume_config_rebuild(
            server,
            operation,
            target,
            ResumeSource::Overview {
                thread: Box::new(thread),
                after,
            },
            plan,
        );
        Ok(AppRunControl::Continue)
    }

    pub(super) async fn retain_overview_origin(&mut self) {
        if let Some(thread_id) = self.current_displayed_thread_id()
            && !self.side_threads.contains_key(&thread_id)
        {
            if let Some(blank) = self.agents_overview.blank_sessions.get_mut(&thread_id)
                && let Some(channel) = self.thread_event_channels.get(&thread_id)
                && let Some(session) = channel.store.lock().await.session.as_ref()
            {
                blank.session = session.clone();
            }
            if let Some(input) = self.chat_widget.capture_thread_input_state() {
                self.agents_overview.input_states.insert(thread_id, input);
            }
        }
        let mut ids = self
            .thread_event_channels
            .keys()
            .copied()
            .collect::<Vec<_>>();
        ids.extend(
            self.agent_navigation
                .tracked_thread_ids()
                .into_iter()
                .filter(|id| !self.thread_event_channels.contains_key(id)),
        );
        let mut running = false;
        for thread_id in &ids {
            if self.side_threads.contains_key(thread_id) {
                continue;
            }
            if let Some(channel) = self.thread_event_channels.get(thread_id) {
                let store = channel.store.lock().await;
                running |= store.active_turn_id().is_some();
                if Some(*thread_id) != self.current_displayed_thread_id()
                    && let Some(input) = store.input_state.clone()
                {
                    self.agents_overview.input_states.insert(*thread_id, input);
                }
                let requests: Vec<_> = store
                    .pending_replay_requests()
                    .into_iter()
                    .filter_map(|request| {
                        self.pending_app_server_requests.buffered_request(request)
                    })
                    .collect();
                if !requests.is_empty() {
                    self.agents_overview
                        .dispatched_requests
                        .entry(*thread_id)
                        .or_default()
                        .extend(requests);
                }
            } else {
                running |= self
                    .agent_navigation
                    .get(thread_id)
                    .is_some_and(|entry| entry.is_running);
            }
        }
        if running {
            for thread_id in ids {
                if !self.side_threads.contains_key(&thread_id) {
                    self.agents_overview
                        .dispatched_requests
                        .entry(thread_id)
                        .or_default();
                }
            }
        }
    }

    pub(super) async fn commit_overview_attachment(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        ready: ResumeReady,
    ) -> Result<()> {
        let ResumeReady {
            context,
            started,
            presentation,
        } = ready;
        let ResumePurpose::Overview {
            thread,
            baseline,
            after,
        } = context.purpose
        else {
            unreachable!("overview commit requires its captured settings policy");
        };
        let thread_id = started.session.thread_id;
        let has_authoritative_mode = started.session.collaboration_mode.is_some();
        let unloaded = matches!(thread.status, api::ThreadStatus::NotLoaded);
        let restored_blank = matches!(presentation, ResumePresentation::CachedBlank);
        let read_only = matches!(presentation, ResumePresentation::ReadOnly { .. });
        let preserve_explicit = unloaded || read_only;
        self.runtime_approvals_reviewer_override = self
            .runtime_approvals_reviewer_override
            .filter(|_| preserve_explicit);
        self.runtime_approval_policy_override =
            self.runtime_approval_policy_override.filter(|policy| {
                preserve_explicit && matches!(policy, RuntimeApprovalPolicyOverride::Explicit(_))
            });
        self.runtime_permission_profile_override = self
            .runtime_permission_profile_override
            .take()
            .filter(|profile| {
                preserve_explicit
                    && profile.turn_override == RuntimePermissionProfileTurnOverride::LegacySandbox
            });
        self.local_settings = context.settings;
        self.config = context.config;
        self.refresh_server_version_overview_notice(CODEX_CLI_VERSION);
        tui.set_notification_settings(&self.local_settings.tui.notification_settings);
        self.file_search
            .update_search_dir(self.config.cwd.to_path_buf());
        if let Err(error) = Box::pin(self.replace_chat_widget_with_app_server_thread(
            tui,
            started,
            ThreadAttachPresentation::SessionLineage,
            None,
        ))
        .await
        {
            self.add_agents_overview_error(format!("Failed to attach to task: {error}"));
            if !read_only && !restored_blank && self.chat_widget.thread_id() != Some(thread_id) {
                self.start_navigation_cleanup(
                    operation,
                    server.request_handle(),
                    thread_id,
                    Vec::new(),
                    CleanupContinuation::Done,
                    false,
                );
            }
            return Ok(());
        }
        if let ResumePresentation::ReadOnly { notice, cause } = &presentation {
            self.apply_resumed_read_only(thread_id, *notice, cause);
        }
        let mut destination = self.chat_widget.config_ref().clone();
        if self.app_server_target.uses_remote_workspace() {
            destination.cwd.clone_from(&self.config.cwd);
            destination
                .workspace_roots
                .clone_from(&self.config.workspace_roots);
            destination
                .permissions
                .set_workspace_roots(self.config.permissions.workspace_roots().to_vec());
        }
        if unloaded {
            let accepted = destination.clone();
            if let Err(error) = self.apply_runtime_policy_overrides(
                &mut destination,
                RuntimePolicyOverrideScope::ExplicitOnly,
            ) {
                self.config = accepted;
                self.chat_widget.add_error_message(format!(
                    "Could not restore the selected permissions: {error:#}"
                ));
                return Ok(());
            }
            self.chat_widget
                .set_approval_policy(destination.permissions.approval_policy.value().into());
            if let Err(error) = self
                .chat_widget
                .set_permission_profile_from_session_snapshot(
                    PermissionProfileSnapshot::from_session_snapshot(
                        destination.permissions.permission_profile().clone(),
                        destination.permissions.active_permission_profile(),
                    ),
                )
            {
                self.chat_widget.add_error_message(format!(
                    "Could not restore the selected permissions: {error}"
                ));
            }
            self.chat_widget
                .set_approvals_reviewer(destination.approvals_reviewer);
            self.chat_widget
                .set_permission_network(destination.permissions.network.clone());
        }
        self.config = destination;
        if !read_only {
            let approval = self.config.permissions.approval_policy.value();
            if self
                .runtime_approval_policy_override
                .is_none_or(|policy| policy.policy().to_core() != approval)
            {
                self.runtime_approval_policy_override = (AskForApproval::from(approval)
                    != baseline.approval)
                    .then_some(RuntimeApprovalPolicyOverride::Restored(approval.into()));
            }
            if self
                .runtime_permission_profile_override
                .as_ref()
                .is_none_or(|profile| !profile.matches_config(&self.config))
            {
                self.runtime_permission_profile_override = (!baseline
                    .permissions
                    .matches_config(&self.config)
                    || baseline.reviewer != self.config.approvals_reviewer)
                    .then(|| RuntimePermissionProfileOverride::from_restored_config(&self.config));
            }
        }
        // Family discovery completes under this owner before the explicit fork continuation.
        // Keep restored source input parked until that continuation owns the fork.
        self.chat_widget.fork_in_progress = matches!(after, SelectionAfter::Fork);
        if let Some(mut input) = self.agents_overview.input_states.remove(&thread_id) {
            presentation.refresh_provider_error(&mut input);
            let pending_settings = restored_blank
                .then(|| input.pending_thread_settings.take())
                .flatten();
            let preserve_in_flight_turn =
                !read_only && self.active_turn_id_for_thread(thread_id).await.is_some();
            let accepted_mode =
                has_authoritative_mode.then(|| self.chat_widget.effective_collaboration_mode());
            let authority = if restored_blank {
                crate::chatwidget::ThreadInputSettingsAuthority::Retained
            } else {
                crate::chatwidget::ThreadInputSettingsAuthority::Session { accepted_mode }
            };
            self.chat_widget.restore_bound_thread_input_state(
                Some(input),
                ThreadInputStateRestoreMode {
                    preserve_in_flight_turn,
                },
                authority,
            );
            if let Some(settings) = pending_settings {
                self.chat_widget.on_thread_settings_updated(settings);
            }
            if !preserve_in_flight_turn && !matches!(after, SelectionAfter::Fork) {
                self.chat_widget.maybe_send_next_queued_input();
            }
        }
        self.queue_farm_receipt_replay();
        operation.intent = NavigationIntent::Resume(ResumeStage::Following {
            thread_id,
            presentation,
            after,
        });
        self.spawn_resume_effect(
            &mut operation,
            server.request_handle(),
            ResumeEffect::Loaded(thread_id),
        );
        self.server_operations.navigation.current = Some(operation);
        Ok(())
    }
}
