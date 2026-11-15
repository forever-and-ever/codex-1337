//! Folder consent for startup and task navigation.
//! Local daemon resumes recheck folders changed during consent. Remote checks
//! retain their existing explicit-cwd scope until authoritative roots are available.

use super::OnboardingResult;
use super::OnboardingScreen;
use super::Step;
use super::run_onboarding_screen;
use crate::AppServerTarget;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::session_preparation::RequestPolicy;
use crate::app_server_session::session_preparation::read_thread;
use crate::config_update::ProjectTrustHost;
use crate::config_update::RemoteProjectTrust;
use crate::config_update::read_remote_project_trust;
use crate::legacy_core::config::Config;
use crate::onboarding::trust_directory::TrustCancelAction;
use crate::onboarding::trust_directory::TrustDirectorySelection;
use crate::onboarding::trust_directory::TrustDirectoryWidget;
use crate::startup_draft::StartupDraftPump;
use crate::tui::Tui;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::Thread;
use codex_protocol::ThreadId;
use codex_protocol::config_types::TrustLevel;
use color_eyre::eyre::Result;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone)]
pub(crate) enum DirectoryTrustConsent {
    Path(PathBuf),
    Key(String),
}
impl DirectoryTrustConsent {
    pub(crate) async fn send(
        self,
        handle: AppServerRequestHandle,
        id: uuid::Uuid,
    ) -> std::result::Result<(), codex_app_server_client::TypedRequestError> {
        match self {
            Self::Path(path) => crate::config_update::write_config_batch_typed(
                handle,
                vec![crate::config_update::trusted_project_edit(&path)],
            )
            .await
            .map(|_| ()),
            Self::Key(key) => {
                let key = key.replace('\\', "\\\\").replace('"', "\\\"");
                handle
                    .request_typed::<serde_json::Value>(
                        codex_app_server_protocol::ClientRequest::ConfigBatchWrite {
                            request_id: codex_app_server_protocol::RequestId::String(format!(
                                "tui-project-trust-write-{id}"
                            )),
                            params: codex_app_server_protocol::ConfigBatchWriteParams {
                                edits: vec![crate::config_update::replace_config_value(
                                    format!("projects.\"{key}\".trust_level"),
                                    serde_json::json!("trusted"),
                                )],
                                file_path: None,
                                expected_version: None,
                                reload_user_config: true,
                            },
                        },
                    )
                    .await
                    .map(|_| ())
            }
        }
    }
}
pub(crate) enum DirectoryTrustDecision {
    Confirmed(OnboardingResult),
    Consent(DirectoryTrustConsent),
}
#[derive(Clone, Copy)]
pub(super) enum TrustPersistence {
    Immediate,
    Deferred,
}
pub(super) fn selected_trust_consent(screen: &OnboardingScreen) -> Option<DirectoryTrustConsent> {
    screen.steps.iter().find_map(|step| {
        if let Step::TrustDirectory(widget) = step
            && widget.selection == Some(TrustDirectorySelection::Trust)
            && !widget.restricted
        {
            Some(match &screen.remote_trust_key {
                Some(key) => DirectoryTrustConsent::Key(key.clone()),
                None => DirectoryTrustConsent::Path(widget.trust_target.clone()),
            })
        } else {
            None
        }
    })
}

#[derive(Clone, Copy, Default)]
pub(crate) struct DirectoryTrustOptions<'a> {
    pub resumed_thread: Option<&'a Thread>,
    pub cancel: Option<TrustCancelAction>,
}

#[derive(Clone)]
pub(crate) struct DirectoryTrustPreparation {
    target: AppServerTarget,
    resumed_thread: Option<ThreadId>,
    cancel: Option<TrustCancelAction>,
    active_untrusted: bool,
    pending_cwds: VecDeque<PathBuf>,
    checked_cwds: Vec<PathBuf>,
    consent: OnboardingResult,
}

impl DirectoryTrustPreparation {
    pub(crate) fn new(
        config: &Config,
        target: &AppServerTarget,
        cwd: &Path,
        options: DirectoryTrustOptions<'_>,
    ) -> Result<Self> {
        let saved_cwd = options
            .resumed_thread
            .filter(|thread| {
                matches!(target, AppServerTarget::LocalDaemon { .. }) && thread.cwd.as_path() != cwd
            })
            .map(|thread| thread.cwd.to_path_buf());
        Ok(Self {
            target: target.clone(),
            resumed_thread: options
                .resumed_thread
                .map(|thread| ThreadId::from_string(&thread.id))
                .transpose()?,
            cancel: options.cancel,
            active_untrusted: config.active_project.is_untrusted(),
            pending_cwds: std::iter::once(cwd.to_path_buf())
                .chain(saved_cwd)
                .collect(),
            checked_cwds: Vec::new(),
            consent: OnboardingResult::default(),
        })
    }

    pub(crate) fn next_cwd(&mut self) -> Option<PathBuf> {
        while let Some(cwd) = self.pending_cwds.pop_front() {
            if !self.checked_cwds.contains(&cwd) {
                self.checked_cwds.push(cwd.clone());
                return Some(cwd);
            }
        }
        None
    }

    pub(crate) fn host(&self) -> ProjectTrustHost {
        if self.target.uses_remote_workspace() {
            ProjectTrustHost::Remote
        } else {
            ProjectTrustHost::Local
        }
    }

    pub(crate) fn accept_project(
        &self,
        project: Option<RemoteProjectTrust>,
    ) -> Result<Option<RemoteProjectTrust>> {
        let Some(project) = project else {
            return Ok(None);
        };
        if self.target.uses_remote_workspace() && project.trust_level == Some(TrustLevel::Untrusted)
        {
            return Ok(None);
        }
        if self.cancel == Some(TrustCancelAction::CurrentTask)
            && project.trust_level == Some(TrustLevel::Untrusted)
            && !self.active_untrusted
        {
            color_eyre::eyre::bail!(
                "Folder trust changed. Reopen the destination with its restricted settings."
            );
        }
        Ok(Some(project))
    }

    fn screen(&self, tui: &Tui, project: RemoteProjectTrust) -> OnboardingScreen {
        let connected = !matches!(self.target, AppServerTarget::Embedded);
        let remote_trust_key =
            connected.then(|| project.trust_target.to_string_lossy().into_owned());
        OnboardingScreen {
            request_frame: tui.frame_requester(),
            steps: vec![Step::TrustDirectory(TrustDirectoryWidget {
                restricted: project.trust_level == Some(TrustLevel::Untrusted),
                existing_task: connected && self.resumed_thread.is_some(),
                cancel: self.cancel.unwrap_or(TrustCancelAction::AgentsOverview),
                cwd: project.cwd,
                trust_target: project.trust_target,
                show_windows_create_sandbox_hint: false,
                should_quit: false,
                selection: None,
                highlighted: TrustDirectorySelection::Trust,
                error: None,
            })],
            remote_trust_key,
            is_done: false,
            should_exit: false,
        }
    }

    pub(crate) async fn prompt(
        &self,
        tui: &mut Tui,
        handle: AppServerRequestHandle,
        project: RemoteProjectTrust,
    ) -> Result<OnboardingResult> {
        let result =
            run_onboarding_screen(self.screen(tui, project), Some(handle), None, tui).await?;
        tui.terminal.clear()?;
        tui.frame_requester().schedule_frame();
        Ok(result)
    }

    pub(crate) async fn choose(
        &self,
        tui: &mut Tui,
        handle: AppServerRequestHandle,
        project: RemoteProjectTrust,
    ) -> Result<DirectoryTrustDecision> {
        let result = super::run_onboarding_screen_with_policy(
            self.screen(tui, project),
            Some(handle),
            None,
            tui,
            TrustPersistence::Deferred,
        )
        .await?;
        tui.terminal.clear()?;
        tui.frame_requester().schedule_frame();
        Ok(result)
    }

    pub(crate) fn note_consent(&mut self, result: OnboardingResult) -> Option<OnboardingResult> {
        if result.should_exit {
            return Some(result);
        }
        self.consent.directory_trust_persisted |= result.directory_trust_persisted;
        None
    }

    pub(crate) fn recheck_thread(&self) -> Option<ThreadId> {
        matches!(self.target, AppServerTarget::LocalDaemon { .. })
            .then_some(self.resumed_thread)
            .flatten()
    }

    pub(crate) fn note_latest_cwd(&mut self, cwd: PathBuf) {
        self.pending_cwds.push_back(cwd);
    }

    pub(crate) fn finish(self) -> OnboardingResult {
        self.consent
    }
}

pub(crate) async fn check_directory_trust(
    tui: &mut Tui,
    app_server: &AppServerSession,
    config: &Config,
    target: &AppServerTarget,
    cwd: &Path,
    options: DirectoryTrustOptions<'_>,
    mut startup_draft: Option<&mut StartupDraftPump>,
) -> Result<OnboardingResult> {
    let mut preparation = DirectoryTrustPreparation::new(config, target, cwd, options)?;
    while let Some(cwd) = preparation.next_cwd() {
        let lookup =
            read_remote_project_trust(app_server.request_handle(), &cwd, preparation.host());
        let project = if let Some(draft) = startup_draft.as_deref_mut() {
            draft.run_until(tui, lookup).await??
        } else {
            lookup.await?
        };
        let Some(project) = preparation.accept_project(project)? else {
            continue;
        };
        if let Some(draft) = startup_draft.as_deref_mut() {
            draft.flush_pending_events(tui).await?;
        }
        let result = preparation
            .prompt(tui, app_server.request_handle(), project)
            .await?;
        if let Some(result) = preparation.note_consent(result) {
            return Ok(result);
        }
        if let Some(thread_id) = preparation.recheck_thread() {
            let handle = app_server.request_handle();
            let recheck = read_thread(RequestPolicy::Unbounded, &handle, thread_id);
            let latest = if let Some(draft) = startup_draft.as_deref_mut() {
                draft.run_until(tui, recheck).await??
            } else {
                recheck.await?
            };
            preparation.note_latest_cwd(latest.cwd.to_path_buf());
        }
    }
    Ok(preparation.finish())
}
