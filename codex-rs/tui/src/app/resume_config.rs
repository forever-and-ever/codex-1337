//! Folder-entry consent and configuration for command-center dispatch and resume.
//! Keeps CLI/runtime cwd precedence, remote-workspace checks, and interactive prompts aligned.
//! Stored-backend settings stay with the candidate; active-backend resume refreshes preferences.

use super::*;
use codex_config::types::ResumeCwdMode;

pub(super) struct ResumeCwdBasis {
    pub(super) mode: Option<ResumeCwdMode>,
    current: PathBuf,
    remembered: PathBuf,
    override_cwd: Option<PathBuf>,
    remote: bool,
}

/// Marks "Current directory" unavailable for remote sessions without a local cwd override.
pub(super) fn resume_current_directory_availability(
    app_server_target: &AppServerTarget,
    environment_manager: &EnvironmentManager,
    cwd_override: Option<&Path>,
) -> crate::config_panel::ResumeCurrentAvailability {
    if crate::uses_remote_workspace_or_environment(app_server_target, environment_manager)
        && cwd_override.is_none()
    {
        crate::config_panel::ResumeCurrentAvailability::RequiresCwdOverride
    } else {
        crate::config_panel::ResumeCurrentAvailability::Available
    }
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
            self.add_agents_overview_error(
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
    ) -> std::result::Result<Box<super::navigation_config::ConfigRebuild>, AppRunControl> {
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
                    self.app_event_tx.send(AppEvent::Exit(ExitMode::Immediate));
                    return Err(AppRunControl::Continue);
                }
                Err(error) => {
                    self.add_agents_overview_error(format!(
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
        Ok(self.capture_config_rebuild(destination, Some(&current)))
    }
}
