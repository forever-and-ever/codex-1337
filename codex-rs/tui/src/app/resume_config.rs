//! Folder-entry consent and configuration for command-center dispatch and resume.
//! Keeps CLI/runtime cwd precedence, remote-workspace checks, and interactive prompts aligned.
//! Carries local preferences alongside the resolved configuration for session replacement.

use super::*;
use crate::onboarding::onboarding_screen::check_directory_trust;
use crate::startup_draft::StartupDraftPump;
use crate::startup_hooks_review::StartupHooksReviewOutcome;
use crate::startup_hooks_review::load_startup_hooks_review_entry;
use crate::startup_hooks_review::maybe_run_startup_hooks_review;
use codex_config::types::ResumeCwdMode;

pub(super) struct ResumeCwdBasis {
    pub(super) mode: Option<ResumeCwdMode>,
    current: PathBuf,
    remembered: PathBuf,
    override_cwd: Option<PathBuf>,
    remote: bool,
}

impl App {
    pub(super) fn capture_resume_cwd_basis(
        &mut self,
        server: &AppServerSession,
    ) -> std::result::Result<ResumeCwdBasis, AppRunControl> {
        let override_cwd = self
            .runtime_working_directory_override
            .as_deref()
            .or(self.harness_overrides.cwd.as_deref())
            .or_else(|| server.remote_cwd_override())
            .map(Path::to_path_buf);
        let mode = crate::session_resume::effective_resume_cwd_mode(
            self.local_settings.tui.resume_cwd,
            override_cwd.as_deref(),
        );
        let remembered = override_cwd
            .clone()
            .unwrap_or_else(|| self.launch_cwd.clone());
        let current = if matches!(mode, Some(ResumeCwdMode::Current)) {
            remembered.clone()
        } else {
            self.config.cwd.to_path_buf()
        };
        let remote = crate::uses_remote_workspace_or_environment(
            &self.app_server_target,
            &self.environment_manager,
        );
        if remote
            && self.harness_overrides.cwd.is_none()
            && server.remote_cwd_override().is_none()
            && matches!(mode, Some(ResumeCwdMode::Current))
        {
            self.add_session_picker_error(
                "`tui.resume_cwd = \"current\"` requires `--cd` when using a remote workspace"
                    .into(),
            );
            return Err(AppRunControl::Continue);
        }
        Ok(ResumeCwdBasis {
            mode,
            current,
            remembered,
            override_cwd,
            remote,
        })
    }

    pub(super) async fn project_resume_config(
        &mut self,
        tui: &mut tui::Tui,
        target: &SessionTarget,
        basis: ResumeCwdBasis,
        history_cwd: Option<PathBuf>,
    ) -> std::result::Result<(Config, crate::local_settings::LocalSettings), AppRunControl> {
        let resume_cwd = if self.app_server_target.uses_remote_workspace() {
            basis.current.clone()
        } else {
            let outcome = crate::session_resume::resolve_cwd_for_resume_or_fork(
                tui,
                &self.config,
                if matches!(basis.mode, Some(ResumeCwdMode::Current)) {
                    None
                } else {
                    history_cwd.or_else(|| target.cwd.clone())
                },
                CwdPromptAction::Resume,
                crate::session_resume::ResumeCwdContext {
                    current_cwd: &basis.current,
                    remembered_current_cwd: &basis.remembered,
                    allow_remember_current: !basis.remote || basis.override_cwd.is_some(),
                    mode: basis.mode,
                },
            )
            .await;
            match outcome {
                Ok(crate::session_resume::ResolveCwdOutcome::Continue(Some(cwd)))
                | Ok(crate::session_resume::ResolveCwdOutcome::ContinueAfterPrompt(cwd)) => cwd,
                Ok(crate::session_resume::ResolveCwdOutcome::Continue(None)) => {
                    basis.current.clone()
                }
                Ok(crate::session_resume::ResolveCwdOutcome::Exit) => {
                    return Err(AppRunControl::Exit(ExitReason::UserRequested));
                }
                Err(error) => {
                    self.add_session_picker_error(format!(
                        "Failed to determine working directory for resume: {error}"
                    ));
                    return Err(AppRunControl::Continue);
                }
            }
        };
        let (current, destination) = if self.app_server_target.uses_remote_workspace() {
            let cwd = self.config.cwd.to_path_buf();
            (cwd.clone(), cwd)
        } else {
            (basis.current, resume_cwd)
        };
        let projected = self
            .rebuild_config_for_resume_or_fallback(&current, destination)
            .await
            .map_err(|error| {
                self.add_session_picker_error(format!(
                    "Failed to rebuild configuration for resume: {error}"
                ));
                AppRunControl::Continue
            })?;
        if self.reject_remote_resume_permission_override(&projected.0) {
            return Err(AppRunControl::Continue);
        }
        Ok(projected)
    }

    pub(super) async fn confirm_directory_trust(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        config: &mut Config,
        cwd: &Path,
        options: crate::onboarding::DirectoryTrustOptions<'_>,
        mut startup_draft: Option<&mut StartupDraftPump>,
    ) -> std::result::Result<Option<crate::local_settings::LocalSettings>, AppRunControl> {
        // Keep the existing explicit remote --cd gate, including retries after cancellation.
        // Other remote destinations await authoritative trust-root metadata.
        let cwd = if self.app_server_target.uses_remote_workspace() {
            let Some(cwd) = app_server.remote_cwd_override() else {
                return Ok(None);
            };
            cwd
        } else {
            cwd
        };
        let result = check_directory_trust(
            tui,
            app_server,
            config,
            &self.app_server_target,
            cwd,
            options,
            startup_draft.as_deref_mut(),
        )
        .await
        .map_err(|error| {
            self.add_session_picker_error(format!("Unable to check folder trust: {error}"));
            AppRunControl::Continue
        })?;
        if result.should_exit {
            if options.cancel == Some(crate::onboarding::TrustCancelAction::CurrentTask) {
                return Err(AppRunControl::Continue);
            }
            if matches!(self.app_server_target, AppServerTarget::Embedded) {
                return Err(AppRunControl::Exit(ExitReason::UserRequested));
            }
            if self
                .chat_widget
                .selected_index_for_present_view(AGENTS_OVERVIEW_VIEW_ID)
                .is_none()
            {
                self.open_agents_overview(app_server);
            }
            return Err(AppRunControl::Continue);
        }
        if !app_server.uses_remote_workspace()
            && (result.directory_trust_persisted
                || options.cancel == Some(crate::onboarding::TrustCancelAction::CurrentTask))
        {
            *config = StartupDraftPump::run_with_optional_draft(
                startup_draft.as_deref_mut(),
                tui,
                self.rebuild_config_for_cwd(config.cwd.to_path_buf()),
            )
            .await
            .map_err(|error| {
                self.add_session_picker_error(format!(
                    "Failed to reload trusted folder settings: {error}"
                ));
                AppRunControl::Continue
            })?;
            // Directory changes review hooks once, after attaching the destination.
            if result.directory_trust_persisted
                && options.resumed_thread.is_none()
                && options.cancel != Some(crate::onboarding::TrustCancelAction::CurrentTask)
            {
                let load_hooks = load_startup_hooks_review_entry(
                    app_server.request_handle(),
                    config.cwd.to_path_buf(),
                );
                let hooks = if let Some(draft) = startup_draft {
                    draft.apply_settings(
                        &crate::local_settings::LocalSettings::from(&*config),
                        config.cwd.as_path(),
                    );
                    async {
                        let hooks = draft.run_until(tui, load_hooks).await?;
                        draft.flush_pending_events(tui).await?;
                        Ok::<_, std::io::Error>(hooks)
                    }
                    .await
                    .map_err(|error| {
                        self.add_session_picker_error(format!(
                            "Unable to load folder hooks: {error}"
                        ));
                        AppRunControl::Continue
                    })?
                } else {
                    load_hooks.await
                };
                match maybe_run_startup_hooks_review(
                    app_server,
                    tui,
                    config,
                    config.bypass_hook_trust,
                    hooks,
                )
                .await
                .map_err(|error| {
                    self.add_session_picker_error(format!(
                        "Unable to review folder hooks: {error}"
                    ));
                    AppRunControl::Continue
                })? {
                    StartupHooksReviewOutcome::Continue => {}
                    StartupHooksReviewOutcome::OpenHooksBrowser(hooks) => {
                        self.chat_widget.open_hooks_browser(hooks);
                        return Err(AppRunControl::Continue);
                    }
                }
            }
            return Ok(Some(self.local_settings.reloaded(config)));
        }
        Ok(None)
    }
}
