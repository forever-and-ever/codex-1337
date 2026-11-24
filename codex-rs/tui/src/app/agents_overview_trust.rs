//! Interactive dashboard consent joins the connection owner; the existing pump owns its draft.

use super::App;
use super::AppRunControl;
use super::AppServerSession;
use super::AppServerTarget;
use super::Config;
use super::ExitReason;
use super::PathBuf;
use super::Result;
use super::StartupDraftPump;
use super::Uuid;
use super::tui;
use crate::config_update::read_remote_project_trust;
use crate::hooks_rpc::HookTrustUpdate;
use crate::hooks_rpc::write_hook_trusts_typed;
use crate::onboarding::onboarding_screen::DirectoryTrustConsent;
use crate::onboarding::onboarding_screen::DirectoryTrustDecision;
use crate::onboarding::onboarding_screen::DirectoryTrustPreparation;
use crate::onboarding::onboarding_screen::OnboardingResult;
use crate::startup_hooks_review::HooksReviewDecision;
use crate::startup_hooks_review::choose_startup_hooks_review;
use crate::startup_hooks_review::hook_trust_updates;
use crate::startup_hooks_review::load_startup_hooks_review_entry;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;

#[derive(Clone)]
pub(in crate::app) struct DashboardPreparation {
    pub(in crate::app) config: Config,
    pub(in crate::app) remote_cwd: Option<PathBuf>,
}

#[derive(Clone)]
pub(in crate::app) enum DashboardTrustAdmission {
    Directory {
        consent: DirectoryTrustConsent,
        preparation: DashboardPreparation,
        plan: DirectoryTrustPreparation,
    },
    Hooks {
        updates: Vec<HookTrustUpdate>,
        preparation: DashboardPreparation,
    },
}

pub(in crate::app) enum DashboardTrustStage {
    Initial(DashboardPreparation),
    Directory {
        preparation: DashboardPreparation,
        plan: DirectoryTrustPreparation,
    },
    ConfirmedHooks(DashboardPreparation),
}

pub(in crate::app) enum DashboardConfigOutcome {
    Ready(Box<Config>, Option<PathBuf>),
    Pending(Box<DashboardTrustAdmission>),
    Stopped(AppRunControl),
}

impl DashboardTrustAdmission {
    pub(in crate::app) fn scope(&self) -> String {
        match self {
            Self::Directory {
                consent: DirectoryTrustConsent::Path(path),
                ..
            } => path.display().to_string(),
            Self::Directory {
                consent: DirectoryTrustConsent::Key(key),
                ..
            } => key.clone(),
            Self::Hooks { preparation, .. } => {
                format!("hooks in {}", preparation.config.cwd.display())
            }
        }
    }

    pub(in crate::app) fn label(&self) -> &'static str {
        match self {
            Self::Directory { .. } => "Saving folder trust",
            Self::Hooks { .. } => "Trusting hooks",
        }
    }

    pub(in crate::app) async fn send(
        &self,
        handle: AppServerRequestHandle,
        id: Uuid,
    ) -> std::result::Result<(), TypedRequestError> {
        match self {
            Self::Directory { consent, .. } => consent.clone().send(handle, id).await,
            Self::Hooks { updates, .. } => write_hook_trusts_typed(handle, updates.clone())
                .await
                .map(|_| ()),
        }
    }

    fn confirmed(&self) -> DashboardTrustStage {
        match self {
            Self::Directory {
                preparation, plan, ..
            } => {
                let mut plan = plan.clone();
                assert!(
                    plan.note_consent(OnboardingResult {
                        directory_trust_persisted: true,
                        selected_gateway: None,
                        should_exit: false
                    })
                    .is_none(),
                    "confirmed consent cannot cancel navigation"
                );
                DashboardTrustStage::Directory {
                    preparation: preparation.clone(),
                    plan,
                }
            }
            Self::Hooks { preparation, .. } => {
                DashboardTrustStage::ConfirmedHooks(preparation.clone())
            }
        }
    }
}

impl App {
    pub(in crate::app) async fn prepare_pumped_dashboard_trust(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        stage: DashboardTrustStage,
        draft: &mut StartupDraftPump,
    ) -> Option<DashboardConfigOutcome> {
        let (mut preparation, mut plan) = match stage {
            DashboardTrustStage::ConfirmedHooks(preparation) => {
                return Box::pin(self.finish_dashboard_session_config(
                    tui,
                    server,
                    preparation,
                    draft,
                ))
                .await
                .map(|(config, cwd)| DashboardConfigOutcome::Ready(Box::new(config), cwd));
            }
            DashboardTrustStage::Directory { preparation, plan } => (preparation, plan),
            DashboardTrustStage::Initial(preparation) => {
                let cwd = if self.app_server_target.uses_remote_workspace() {
                    let Some(cwd) = server.remote_cwd_override() else {
                        return Box::pin(self.finish_dashboard_session_config(
                            tui,
                            server,
                            preparation,
                            draft,
                        ))
                        .await
                        .map(|(config, cwd)| DashboardConfigOutcome::Ready(Box::new(config), cwd));
                    };
                    cwd
                } else {
                    preparation.config.cwd.as_path()
                };
                let plan = match DirectoryTrustPreparation::new(
                    &preparation.config,
                    &self.app_server_target,
                    cwd,
                    crate::onboarding::DirectoryTrustOptions::default(),
                ) {
                    Ok(plan) => plan,
                    Err(error) => {
                        self.add_agents_overview_error(format!(
                            "Unable to check folder trust: {error}"
                        ));
                        return None;
                    }
                };
                (preparation, plan)
            }
        };
        while let Some(cwd) = plan.next_cwd() {
            let lookup = read_remote_project_trust(server.request_handle(), &cwd, plan.host());
            let project = match Box::pin(StartupDraftPump::run_with_optional_draft(
                Some(&mut *draft),
                tui,
                lookup,
            ))
            .await
            {
                Ok(project) => project,
                Err(error) => {
                    self.add_agents_overview_error(format!(
                        "Unable to check folder trust: {error}"
                    ));
                    return None;
                }
            };
            let project = match plan.accept_project(project) {
                Ok(Some(project)) => project,
                Ok(None) => continue,
                Err(error) => {
                    self.add_agents_overview_error(format!(
                        "Unable to check folder trust: {error}"
                    ));
                    return None;
                }
            };
            if let Err(error) = draft.flush_pending_events(tui).await {
                self.add_agents_overview_error(format!("Unable to retain the draft: {error}"));
                return None;
            }
            let decision = match plan.choose(tui, server.request_handle(), project).await {
                Ok(decision) => decision,
                Err(error) => {
                    self.add_agents_overview_error(format!(
                        "Unable to check folder trust: {error}"
                    ));
                    return None;
                }
            };
            match decision {
                DirectoryTrustDecision::Consent(consent) => {
                    return Some(DashboardConfigOutcome::Pending(Box::new(
                        DashboardTrustAdmission::Directory {
                            consent,
                            preparation,
                            plan,
                        },
                    )));
                }
                DirectoryTrustDecision::Confirmed(consent) => {
                    if let Some(consent) = plan.note_consent(consent) {
                        if consent.should_exit
                            && matches!(self.app_server_target, AppServerTarget::Embedded)
                        {
                            return Some(DashboardConfigOutcome::Stopped(AppRunControl::Exit(
                                ExitReason::UserRequested,
                            )));
                        }
                        if consent.should_exit {
                            self.open_agents_overview(server);
                        }
                        return Some(DashboardConfigOutcome::Stopped(AppRunControl::Continue));
                    }
                }
            }
        }
        let consent = plan.finish();
        if consent.directory_trust_persisted && !server.uses_remote_workspace() {
            preparation.config = match StartupDraftPump::run_with_optional_draft(
                Some(&mut *draft),
                tui,
                self.rebuild_config_for_cwd(preparation.config.cwd.to_path_buf()),
            )
            .await
            {
                Ok(config) => config,
                Err(error) => {
                    self.add_agents_overview_error(format!(
                        "Failed to reload trusted folder settings: {error}"
                    ));
                    return None;
                }
            };
            let load = load_startup_hooks_review_entry(
                server.request_handle(),
                preparation.config.cwd.to_path_buf(),
            );
            let hooks = match Box::pin(StartupDraftPump::run_with_optional_draft(
                Some(&mut *draft),
                tui,
                async { Ok::<_, std::io::Error>(load.await) },
            ))
            .await
            {
                Ok(hooks) => hooks,
                Err(error) => {
                    self.add_agents_overview_error(format!("Unable to load folder hooks: {error}"));
                    return None;
                }
            };
            if let Err(error) = draft.flush_pending_events(tui).await {
                self.add_agents_overview_error(format!("Unable to retain the draft: {error}"));
                return None;
            }
            match choose_startup_hooks_review(tui, &preparation.config, hooks, None).await {
                Ok(HooksReviewDecision::Continue) => {}
                Ok(HooksReviewDecision::OpenHooksBrowser(hooks)) => {
                    self.chat_widget.open_hooks_browser(hooks);
                    return None;
                }
                Ok(HooksReviewDecision::TrustAll(hooks)) => {
                    return Some(DashboardConfigOutcome::Pending(Box::new(
                        DashboardTrustAdmission::Hooks {
                            updates: hook_trust_updates(&hooks),
                            preparation,
                        },
                    )));
                }
                Err(error) => {
                    self.add_agents_overview_error(format!(
                        "Unable to review folder hooks: {error}"
                    ));
                    return None;
                }
            }
        }
        Box::pin(self.finish_dashboard_session_config(tui, server, preparation, draft))
            .await
            .map(|(config, cwd)| DashboardConfigOutcome::Ready(Box::new(config), cwd))
    }

    pub(in crate::app) async fn continue_confirmed_dashboard_trust(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        consent: &DashboardTrustAdmission,
    ) -> Result<AppRunControl> {
        let Some(mut draft) = self.agents_overview.new_session_draft.take() else {
            self.chat_widget.add_info_message(
                format!(
                    "Trust saved for {}. The original dashboard draft is no longer open.",
                    consent.scope()
                ),
                None,
            );
            return Ok(AppRunControl::Continue);
        };
        let previous = self.current_displayed_thread_id();
        let outcome = Box::pin(self.prepare_pumped_dashboard_trust(
            tui,
            server,
            consent.confirmed(),
            &mut draft,
        ))
        .await;
        let result = match outcome {
            Some(DashboardConfigOutcome::Ready(config, cwd)) => {
                Box::pin(self.start_dashboard_with_config(tui, server, *config, cwd, &mut draft))
                    .await
            }
            Some(DashboardConfigOutcome::Pending(consent)) => {
                self.admit_dashboard_trust(server, consent);
                Ok(AppRunControl::Continue)
            }
            Some(DashboardConfigOutcome::Stopped(control)) => Ok(control),
            None => Ok(AppRunControl::Continue),
        };
        self.finish_dashboard_draft(tui, previous, draft).await?;
        result
    }
}
