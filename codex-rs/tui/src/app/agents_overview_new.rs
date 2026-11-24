//! Creates empty sessions from the command center without interrupting other agents.

#[path = "agents_overview_new/admission.rs"]
mod admission;

use super::*;
use crate::app_server_session::is_server_shutting_down_error;
use crate::startup_draft::StartupDraftInitialScreen;
use crate::startup_draft::StartupDraftPump;
use crate::startup_draft::StartupDraftSessionAction;

impl App {
    pub(in crate::app) async fn new_agents_overview_session(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        cwd: Option<AbsolutePathBuf>,
    ) -> Result<AppRunControl> {
        if self.reconnect.offline || self.windows_sandbox_blocks_thread_switch() {
            return Ok(AppRunControl::Continue);
        }
        if cwd.is_none() && self.restore_parked_session_draft(tui, app_server).await? {
            return Ok(AppRunControl::Continue);
        }
        let std::ops::ControlFlow::Continue(cwd) = self.admit_overview_session(app_server, cwd)
        else {
            return Ok(AppRunControl::Continue);
        };
        if !self.pumped_configuration_ready() {
            return Ok(AppRunControl::Continue);
        }
        self.supersede_pending_navigation();
        let previous_thread = self.current_displayed_thread_id();
        let mut draft = self
            .agents_overview
            .new_session_draft
            .take()
            .unwrap_or_else(|| {
                Box::new(StartupDraftPump::new(
                    tui,
                    StartupDraftInitialScreen::Composer,
                    StartupDraftSessionAction::NewFromCommandCenter,
                ))
            });
        let display_cwd = cwd
            .as_ref()
            .map(AbsolutePathBuf::to_path_buf)
            .unwrap_or_else(|| self.chat_widget.session_cwd().to_path_buf());
        draft.apply_settings(
            &crate::local_settings::LocalSettings::from(self.chat_widget.config_ref()),
            &display_cwd,
        );
        draft.show(tui)?;
        // Keep the large session-start future off the TUI's stack in dev builds.
        let result =
            Box::pin(self.start_agents_overview_session(tui, app_server, cwd, &mut draft)).await;
        self.finish_dashboard_draft(tui, previous_thread, draft)
            .await?;
        result
    }

    pub(in crate::app) async fn finish_dashboard_draft(
        &mut self,
        tui: &mut tui::Tui,
        previous_thread: Option<ThreadId>,
        mut draft: Box<StartupDraftPump>,
    ) -> Result<()> {
        let attached = self.current_displayed_thread_id() != previous_thread;
        let flushed = if attached {
            draft.flush_pending_paste_newline(tui).await
        } else {
            draft.flush_pending_events(tui).await
        };
        if let Err(error) = flushed {
            self.agents_overview.new_session_draft = Some(draft);
            return Err(error.into());
        }
        if attached {
            self.chat_widget
                .empty_state_animation
                .borrow_mut()
                .continue_from(&mut draft.blossom.borrow_mut());
            self.chat_widget.restore_startup_draft(draft.take_draft());
        } else {
            self.agents_overview.new_session_draft = Some(draft);
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    pub(in crate::app) async fn start_agents_overview_session(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        cwd: Option<AbsolutePathBuf>,
        startup_draft: &mut StartupDraftPump,
    ) -> Result<AppRunControl> {
        if self.reconnect.offline || self.windows_sandbox_blocks_thread_switch() {
            return Ok(AppRunControl::Continue);
        }
        let Some(outcome) = self
            .agents_overview_session_config(tui, app_server, cwd, startup_draft)
            .await
        else {
            return Ok(AppRunControl::Continue);
        };
        match outcome {
            trust::DashboardConfigOutcome::Pending(consent) => {
                self.admit_dashboard_trust(app_server, consent);
                Ok(AppRunControl::Continue)
            }
            trust::DashboardConfigOutcome::Stopped(control) => Ok(control),
            trust::DashboardConfigOutcome::Ready(config, remote_cwd) => {
                self.start_dashboard_with_config(
                    tui,
                    app_server,
                    *config,
                    remote_cwd,
                    startup_draft,
                )
                .await
            }
        }
    }

    pub(in crate::app) async fn start_dashboard_with_config(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        config: Config,
        remote_cwd: Option<PathBuf>,
        startup_draft: &mut StartupDraftPump,
    ) -> Result<AppRunControl> {
        let selected_profile = self.chat_widget.thread_id().and_then(|thread_id| {
            if let Some(selection) = self
                .agents_overview
                .requested_permission_profiles
                .get(&thread_id)
            {
                return Some(selection.clone());
            }
            let source = self.chat_widget.config_ref();
            let active = source.permissions.active_permission_profile()?;
            (self
                .agents_overview
                .selected_permission_profiles
                .get(&thread_id)
                == Some(&active.id))
            .then(|| PermissionProfileSelection {
                profile_id: active.id.clone(),
                approval_policy: Some(source.permissions.approval_policy.value().into()),
                approvals_reviewer: Some(source.approvals_reviewer),
                display_label: active.id,
            })
        });
        let local_settings = self.local_settings.reloaded(&config);
        startup_draft.apply_settings(&local_settings, config.cwd.as_path());
        let result = StartupDraftPump::run_with_optional_draft(
            Some(&mut *startup_draft),
            tui,
            Box::pin(app_server.start_thread_with_session_start_source(
                &local_settings,
                &config,
                /*session_start_source*/ None,
                remote_cwd.as_deref(),
                selected_profile.as_ref(),
            )),
        )
        .await;
        let started = match result {
            Ok(started) => started,
            Err(error) => {
                let (title, message) = if matches!(
                    error.downcast_ref::<TypedRequestError>(),
                    Some(TypedRequestError::Server { source, .. })
                        if is_server_shutting_down_error(source)
                ) {
                    let mut message = String::from(
                        "New sessions are paused while active turns finish or are canceled. The default shutdown wait is up to one minute.",
                    );
                    if !matches!(self.app_server_target, AppServerTarget::Embedded) {
                        message.push_str(
                            " If the server restarts, Codex will reconnect automatically.",
                        );
                    }
                    (Some("Codex's background server is shutting down"), message)
                } else {
                    (None, format!("Failed to start session: {error}"))
                };
                self.show_agents_overview_error(title, message);
                return Ok(AppRunControl::Continue);
            }
        };
        let thread_id = started.session.thread_id;
        if let Some(selected) = selected_profile {
            self.agents_overview
                .selected_permission_profiles
                .insert(thread_id, selected.profile_id);
        }
        self.agents_overview
            .blank_sessions
            .insert(thread_id, started.clone());
        // Use the dashboard's existing attachment path, which preserves running agents
        // and unsent input in the previous session. Do not send an initial turn.
        let control = Box::pin(self.attach_new_agents_overview_thread(
            tui,
            app_server,
            thread_id,
            (config, started),
        ))
        .await;
        if self.current_displayed_thread_id() != Some(thread_id) {
            self.agents_overview.blank_sessions.remove(&thread_id);
            self.adopt_navigation_cleanup(app_server, thread_id);
        }
        control
    }
}
