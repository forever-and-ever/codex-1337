use super::ACK_TIMEOUT;
use super::AGENTS_OVERVIEW_VIEW_ID;
use super::AbortOnDropHandle;
use super::App;
use super::AppRunControl;
use super::AppServerSession;
use super::AppServerStartedThread;
use super::AppServerTarget;
use super::ApprovalsReviewer;
use super::AskForApproval;
use super::CODEX_CLI_VERSION;
use super::CleanupContinuation;
use super::ClientRequest;
use super::Config;
use super::DetachScope;
use super::ExitReason;
use super::FutureExt;
use super::HooksListEntry;
use super::LaneState;
use super::NavigationCompletion;
use super::NavigationIntent;
use super::NavigationReceipt;
use super::PathBuf;
use super::PendingNavigation;
use super::PermissionProfileSnapshot;
use super::Result;
use super::RuntimeApprovalPolicyOverride;
use super::RuntimePermissionProfileOverride;
use super::RuntimePermissionProfileTurnOverride;
use super::RuntimePolicyOverrideScope;
use super::SelectionAfter;
use super::ServerCompletion;
use super::SessionTarget;
use super::ThreadId;
use super::TurnStatus;
use super::detach;
use super::navigation_config;
use super::trust::ResumeTrustConsent;
use super::tui;
use crate::app::resume_config::ResumeCwdBasis;
use crate::app::session_lifecycle::ThreadAttachPresentation;
use crate::app_server_session::session_preparation::PreparedSession;
use crate::app_server_session::session_preparation::PreparedView;
use crate::app_server_session::session_preparation::RequestPolicy;
use crate::app_server_session::session_preparation::ResumePreparation;
use crate::app_server_session::session_preparation::SubscriptionCustody;
use crate::app_server_session::session_preparation::ViewPreparation;
use crate::app_server_session::session_preparation::{self as preparation};
use crate::chatwidget::ThreadInputStateRestoreMode;
use crate::config_update::ProjectTrustHost;
use crate::config_update::RemoteProjectTrust;
use crate::config_update::read_remote_project_trust_with_policy;
use crate::local_settings::LocalSettings;
use crate::onboarding::onboarding_screen::DirectoryTrustDecision;
use crate::onboarding::onboarding_screen::DirectoryTrustPreparation;
use crate::startup_hooks_review::HooksReviewDecision;
use crate::startup_hooks_review::choose_startup_hooks_review;
use crate::startup_hooks_review::hook_trust_updates;
use crate::startup_hooks_review::load_startup_hooks_review_entry;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;
use codex_config::types::ResumeCwdMode;

#[path = "server_overview_navigation.rs"]
mod overview;

#[derive(Clone)]
pub(super) struct ResumeContext {
    target: SessionTarget,
    config: Config,
    settings: LocalSettings,
    purpose: ResumePurpose,
}

#[derive(Clone)]
pub(super) enum ResumeSource {
    Explicit,
    Overview {
        thread: Box<api::Thread>,
        after: SelectionAfter,
    },
}

#[derive(Clone)]
pub(super) enum ResumePurpose {
    Explicit,
    Overview {
        thread: Box<api::Thread>,
        baseline: Box<OverviewBaseline>,
        after: SelectionAfter,
    },
}

#[derive(Clone)]
pub(super) struct OverviewBaseline {
    approval: AskForApproval,
    reviewer: ApprovalsReviewer,
    permissions: RuntimePermissionProfileOverride,
}

impl OverviewBaseline {
    fn capture(config: &Config) -> Self {
        Self {
            approval: config.permissions.approval_policy.value().into(),
            reviewer: config.approvals_reviewer,
            permissions: RuntimePermissionProfileOverride::from_config(config),
        }
    }
}

impl ResumePurpose {
    fn from_source(source: ResumeSource, config: &Config) -> Self {
        match source {
            ResumeSource::Explicit => Self::Explicit,
            ResumeSource::Overview { thread, after } => Self::Overview {
                thread,
                after,
                baseline: Box::new(OverviewBaseline::capture(config)),
            },
        }
    }
}

impl ResumeContext {
    pub(super) fn hook_scope(&self) -> String {
        format!("hooks in {}", self.config.cwd.to_path_buf().display())
    }
}

pub(super) struct ResumeReady {
    context: ResumeContext,
    started: AppServerStartedThread,
    presentation: ResumePresentation,
}

pub(super) enum ReadOnlyCause {
    ExternalWriter,
    Provider(api::ModelProviderError),
}

pub(super) enum ResumePresentation {
    Writable,
    CachedBlank,
    ReadOnly {
        notice: Option<&'static str>,
        cause: ReadOnlyCause,
    },
}

impl ResumePresentation {
    fn refresh_provider_error(&self, input: &mut crate::chatwidget::ThreadInputState) {
        match self {
            Self::Writable
            | Self::ReadOnly {
                cause: ReadOnlyCause::ExternalWriter,
                ..
            } => {
                input.provider_error = None;
            }
            Self::ReadOnly {
                cause: ReadOnlyCause::Provider(error),
                ..
            } => {
                input.provider_error = Some(error.clone());
            }
            Self::CachedBlank => {}
        }
    }
}

pub(super) enum ResumeStage {
    TrustedConfig(Box<ResumeContext>),
    Rebuilding {
        target: SessionTarget,
        source: ResumeSource,
    },
    Admitted(SessionTarget),
    OverviewAdmitted {
        thread_id: ThreadId,
        after: SelectionAfter,
    },
    OverviewConfig {
        thread_id: ThreadId,
        after: SelectionAfter,
    },
    OverviewMetadata {
        thread_id: ThreadId,
        after: SelectionAfter,
    },
    Config(SessionTarget),
    Cwd {
        target: SessionTarget,
        basis: ResumeCwdBasis,
        source: ResumeSource,
    },
    TrustMetadata(Box<ResumeContext>),
    Trust {
        context: Box<ResumeContext>,
        plan: DirectoryTrustPreparation,
    },
    Hooks(Box<ResumeContext>),
    Acquiring(Box<ResumeContext>),
    RestoringBlank(Box<ResumeContext>),
    Viewing {
        context: Box<ResumeContext>,
        cause: ReadOnlyCause,
    },
    Detaching {
        ready: Box<ResumeReady>,
        plan: detach::DetachPlan,
    },
    Following {
        thread_id: ThreadId,
        presentation: ResumePresentation,
        after: SelectionAfter,
    },
    Goal(ThreadId),
}

impl ResumeStage {
    pub(super) fn thread_id(&self) -> ThreadId {
        match self {
            Self::OverviewAdmitted { thread_id, .. }
            | Self::OverviewConfig { thread_id, .. }
            | Self::OverviewMetadata { thread_id, .. }
            | Self::Following { thread_id, .. }
            | Self::Goal(thread_id) => *thread_id,
            Self::Admitted(target)
            | Self::Config(target)
            | Self::Cwd { target, .. }
            | Self::Rebuilding { target, .. } => target.thread_id,
            Self::TrustedConfig(context)
            | Self::TrustMetadata(context)
            | Self::Trust { context, .. }
            | Self::Hooks(context)
            | Self::Acquiring(context)
            | Self::RestoringBlank(context)
            | Self::Viewing { context, .. } => context.target.thread_id,
            Self::Detaching { ready, .. } => ready.started.session.thread_id,
        }
    }

    pub(super) fn is_read_only(&self) -> bool {
        !matches!(self, Self::Acquiring(_) | Self::Detaching { .. })
    }

    pub(super) fn pending_fork_source(&self) -> Option<ThreadId> {
        match self {
            Self::Following {
                thread_id,
                after: SelectionAfter::Fork,
                ..
            } => Some(*thread_id),
            _ => None,
        }
    }

    pub(super) fn fences_thread(&self, thread_id: ThreadId) -> bool {
        self.thread_id() == thread_id
            || matches!(self, Self::Detaching { plan, .. } if plan.targets.iter().any(|target| target.thread_id == thread_id))
    }
}

pub(super) struct ResumeReceipt {
    handle: AppServerRequestHandle,
    outcome: ResumeOutcome,
}

impl ResumeReceipt {
    pub(super) fn requires_owner(&self) -> bool {
        matches!(
            self.outcome,
            ResumeOutcome::Acquired(_) | ResumeOutcome::Detached(_)
        )
    }
}

enum ResumeOutcome {
    Refreshed(Result<Box<(Config, LocalSettings)>>),
    Rebuilt(Result<Box<(Config, LocalSettings)>>),
    Thread(Result<api::Thread>),
    Project(Result<Option<RemoteProjectTrust>>),
    Rechecked(Result<api::Thread>),
    Hooks(Result<HooksListEntry, String>),
    Acquired(PreparedSession),
    Cached(AppServerStartedThread),
    Viewed(PreparedView),
    Detached(detach::DetachReceipt),
    Loaded(Result<Vec<(ThreadId, Result<api::Thread>)>>),
    Goal(Result<api::ThreadGoalGetResponse>),
    Terminated,
}

enum ResumeEffect {
    Refresh(Box<navigation_config::ConfigRebuild>),
    Rebuild(Box<navigation_config::ConfigRebuild>),
    Thread(ThreadId),
    Project {
        cwd: PathBuf,
        host: ProjectTrustHost,
    },
    Recheck(ThreadId),
    Hooks(PathBuf),
    Acquire(Box<ResumePreparation>),
    Cached(Box<AppServerStartedThread>),
    View(Box<ViewPreparation>),
    Detach(detach::DetachPlan),
    Loaded(ThreadId),
    Goal(ThreadId),
}

impl App {
    pub(in crate::app) fn enqueue_session_resume(
        &mut self,
        server: &AppServerSession,
        target: SessionTarget,
    ) {
        self.enqueue_navigation(
            server,
            NavigationIntent::Resume(ResumeStage::Admitted(target)),
        );
    }

    pub(super) fn dispatch_session_resume(
        &self,
        server: &AppServerSession,
        operation: &mut PendingNavigation,
    ) {
        let stage = match std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        {
            NavigationIntent::Resume(ResumeStage::Admitted(target)) => ResumeStage::Config(target),
            NavigationIntent::Resume(ResumeStage::OverviewAdmitted { thread_id, after }) => {
                ResumeStage::OverviewConfig { thread_id, after }
            }
            _ => unreachable!("resume dispatch requires admitted target"),
        };
        operation.intent = NavigationIntent::Resume(stage);
        self.spawn_resume_effect(
            operation,
            server.request_handle(),
            ResumeEffect::Refresh(
                self.capture_config_rebuild(self.chat_widget.config_ref().cwd.to_path_buf(), None),
            ),
        );
    }

    fn spawn_resume_effect(
        &self,
        operation: &mut PendingNavigation,
        handle: AppServerRequestHandle,
        effect: ResumeEffect,
    ) {
        let completions = self.server_operations.control_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            let outcome = Box::pin(
                std::panic::AssertUnwindSafe(async {
                    match effect {
                        ResumeEffect::Refresh(plan) => ResumeOutcome::Refreshed(plan.run().await),
                        ResumeEffect::Rebuild(plan) => ResumeOutcome::Rebuilt(plan.run().await),
                        ResumeEffect::Thread(thread_id) => ResumeOutcome::Thread(
                            preparation::read_thread(
                                RequestPolicy::Interactive,
                                &handle,
                                thread_id,
                            )
                            .await,
                        ),
                        ResumeEffect::Recheck(thread_id) => ResumeOutcome::Rechecked(
                            preparation::read_thread(
                                RequestPolicy::Interactive,
                                &handle,
                                thread_id,
                            )
                            .await,
                        ),
                        ResumeEffect::Project { cwd, host } => ResumeOutcome::Project(
                            read_remote_project_trust_with_policy(
                                RequestPolicy::Interactive,
                                handle.clone(),
                                &cwd,
                                host,
                            )
                            .await,
                        ),
                        ResumeEffect::Hooks(cwd) => ResumeOutcome::Hooks(
                            tokio::time::timeout(
                                ACK_TIMEOUT,
                                load_startup_hooks_review_entry(handle.clone(), cwd),
                            )
                            .await
                            .map_err(|_| "Folder hooks read timed out".into()),
                        ),
                        ResumeEffect::Acquire(plan) => {
                            ResumeOutcome::Acquired(Box::pin(plan.run()).await)
                        }
                        ResumeEffect::Cached(started) => ResumeOutcome::Cached(*started),
                        ResumeEffect::View(plan) => {
                            ResumeOutcome::Viewed(Box::pin(plan.run()).await)
                        }
                        ResumeEffect::Detach(plan) => ResumeOutcome::Detached(
                            match tokio::time::timeout(
                                ACK_TIMEOUT,
                                detach::send(&handle, id, &plan),
                            )
                            .await
                            {
                                Ok(results) => detach::DetachReceipt::Complete(results),
                                Err(_) => detach::DetachReceipt::BudgetExpired,
                            },
                        ),
                        ResumeEffect::Loaded(root) => ResumeOutcome::Loaded(
                            preparation::loaded_observations(
                                RequestPolicy::Interactive,
                                &handle,
                                root,
                            )
                            .await,
                        ),
                        ResumeEffect::Goal(thread_id) => ResumeOutcome::Goal(
                            preparation::request(
                                RequestPolicy::Interactive,
                                &handle,
                                ClientRequest::ThreadGoalGet {
                                    request_id: api::RequestId::String(format!(
                                        "tui-navigation-{id}-goal"
                                    )),
                                    params: api::ThreadGoalGetParams {
                                        thread_id: thread_id.to_string(),
                                    },
                                },
                            )
                            .await
                            .map_err(Into::into),
                        ),
                    }
                })
                .catch_unwind(),
            )
            .await
            .unwrap_or(ResumeOutcome::Terminated);
            let _ = completions.send(ServerCompletion::Navigation(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::Resumed(ResumeReceipt { handle, outcome }),
                },
            )));
        })));
    }

    fn begin_resume_trust(
        &mut self,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        context: Box<ResumeContext>,
        resumed_thread: Option<&api::Thread>,
    ) -> AppRunControl {
        let cwd = if self.app_server_target.uses_remote_workspace() {
            let Some(cwd) = server.remote_cwd_override() else {
                self.acquire_resumed_thread(server, operation, context);
                return AppRunControl::Continue;
            };
            cwd
        } else {
            context.config.cwd.as_path()
        };
        let plan = match DirectoryTrustPreparation::new(
            &context.config,
            &self.app_server_target,
            cwd,
            crate::onboarding::DirectoryTrustOptions {
                resumed_thread,
                ..Default::default()
            },
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.add_agents_overview_error(format!("Unable to check folder trust: {error}"));
                return AppRunControl::Continue;
            }
        };
        self.advance_resume_trust(server, &mut operation, context, plan);
        if matches!(operation.intent, NavigationIntent::Resume(_)) {
            self.server_operations.navigation.current = Some(operation);
        }
        AppRunControl::Continue
    }

    fn advance_resume_trust(
        &mut self,
        server: &mut AppServerSession,
        operation: &mut PendingNavigation,
        context: Box<ResumeContext>,
        mut plan: DirectoryTrustPreparation,
    ) {
        if let Some(cwd) = plan.next_cwd() {
            let host = plan.host();
            operation.intent = NavigationIntent::Resume(ResumeStage::Trust { context, plan });
            self.spawn_resume_effect(
                operation,
                server.request_handle(),
                ResumeEffect::Project { cwd, host },
            );
            return;
        }
        let consent = plan.finish();
        if !server.uses_remote_workspace() && consent.directory_trust_persisted {
            let rebuild = self.capture_config_rebuild(context.config.cwd.to_path_buf(), None);
            operation.intent = NavigationIntent::Resume(ResumeStage::TrustedConfig(context));
            self.spawn_resume_effect(
                operation,
                server.request_handle(),
                ResumeEffect::Rebuild(rebuild),
            );
            return;
        }
        self.prepare_resume_acquisition(server, operation, context);
    }

    async fn review_resume_hooks(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        operation: PendingNavigation,
        context: Box<ResumeContext>,
        hooks: HooksListEntry,
    ) -> Result<()> {
        let decision = match choose_startup_hooks_review(tui, &context.config, hooks, None).await {
            Ok(decision) => decision,
            Err(error) => {
                self.add_agents_overview_error(format!("Unable to review folder hooks: {error}"));
                return Ok(());
            }
        };
        match decision {
            HooksReviewDecision::Continue => {
                self.acquire_resumed_thread(server, operation, context);
            }
            HooksReviewDecision::OpenHooksBrowser(hooks) => {
                self.chat_widget.open_hooks_browser(hooks);
            }
            HooksReviewDecision::TrustAll(hooks) => self.transfer_navigation_trust(
                server,
                operation,
                ResumeTrustConsent::Hooks {
                    updates: hook_trust_updates(&hooks),
                    context,
                },
            ),
        }
        Ok(())
    }

    pub(super) fn continue_resume_trust_saved(
        &mut self,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        consent: &ResumeTrustConsent,
    ) {
        match consent {
            ResumeTrustConsent::Hooks { context, .. } => {
                self.acquire_resumed_thread(server, operation, context.clone());
            }
            ResumeTrustConsent::Directory { context, plan, .. } => {
                let context = context.clone();
                let mut plan = plan.clone();
                let exit =
                    plan.note_consent(crate::onboarding::onboarding_screen::OnboardingResult {
                        directory_trust_persisted: true,
                        selected_gateway: None,
                        should_exit: false,
                    });
                assert!(
                    exit.is_none(),
                    "confirmed trust write cannot cancel its consent"
                );
                if let Some(thread_id) = plan.recheck_thread() {
                    operation.intent =
                        NavigationIntent::Resume(ResumeStage::Trust { context, plan });
                    self.spawn_resume_effect(
                        &mut operation,
                        server.request_handle(),
                        ResumeEffect::Recheck(thread_id),
                    );
                    self.server_operations.navigation.current = Some(operation);
                } else {
                    self.advance_resume_trust(server, &mut operation, context, plan);
                    if matches!(operation.intent, NavigationIntent::Resume(_)) {
                        self.server_operations.navigation.current = Some(operation);
                    }
                }
            }
        }
    }

    fn prepare_resume_acquisition(
        &mut self,
        server: &mut AppServerSession,
        operation: &mut PendingNavigation,
        mut context: Box<ResumeContext>,
    ) {
        let loaded_overview = matches!(&context.purpose, ResumePurpose::Overview { thread, .. } if !matches!(thread.status, api::ThreadStatus::NotLoaded));
        if let ResumePurpose::Overview { baseline, .. } = &mut context.purpose {
            **baseline = OverviewBaseline::capture(&context.config);
        }
        if !loaded_overview {
            if let Err(error) = self.apply_runtime_policy_overrides(
                &mut context.config,
                RuntimePolicyOverrideScope::ExplicitOnly,
            ) {
                self.add_agents_overview_error(format!("{error:#}"));
                return;
            }
            if matches!(context.purpose, ResumePurpose::Overview { .. })
                && (matches!(self.runtime_approval_policy_override, Some(RuntimeApprovalPolicyOverride::Explicit(policy)) if policy.to_core() != context.config.permissions.approval_policy.value())
                    || self
                        .runtime_permission_profile_override
                        .as_ref()
                        .is_some_and(|profile| {
                            profile.turn_override
                                == RuntimePermissionProfileTurnOverride::LegacySandbox
                                && !profile.matches_config(&context.config)
                        }))
            {
                self.add_agents_overview_error(
                    "Cannot resume task without preserving the selected permissions.".into(),
                );
                return;
            }
        }
        if let Some(mode) = context.target.history_mode {
            server.remember_thread_history_mode(context.target.thread_id, mode);
        }
        if loaded_overview
            && let Some(blank) = self
                .agents_overview
                .blank_sessions
                .get(&context.target.thread_id)
        {
            let mut started = blank.clone();
            if let ResumePurpose::Overview { thread, .. } = &context.purpose {
                started.session.thread_name.clone_from(&thread.name);
            }
            operation.intent = NavigationIntent::Resume(ResumeStage::RestoringBlank(context));
            self.spawn_resume_effect(
                operation,
                server.request_handle(),
                ResumeEffect::Cached(Box::new(started)),
            );
            return;
        }
        let model = if loaded_overview {
            crate::app_server_session::ResumeModelSettings::PreserveExistingThread
        } else {
            self.resume_model_settings()
        };
        let plan = server.prepare_resume_thread(
            RequestPolicy::Interactive,
            &context.settings,
            context.config.clone(),
            context.target.thread_id,
            model,
            self.resume_permission_overrides(&context.config),
        );
        operation.intent = NavigationIntent::Resume(ResumeStage::Acquiring(context));
        self.spawn_resume_effect(
            operation,
            server.request_handle(),
            ResumeEffect::Acquire(Box::new(plan)),
        );
    }

    fn acquire_resumed_thread(
        &mut self,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        context: Box<ResumeContext>,
    ) {
        self.prepare_resume_acquisition(server, &mut operation, context);
        if matches!(operation.intent, NavigationIntent::Resume(_)) {
            self.server_operations.navigation.current = Some(operation);
        }
    }

    async fn detach_for_resume(
        &mut self,
        server: &AppServerSession,
        mut operation: PendingNavigation,
        ready: Box<ResumeReady>,
    ) {
        if matches!(ready.context.purpose, ResumePurpose::Overview { .. }) {
            self.retain_overview_origin().await;
        }
        let plan = self
            .prepare_detach_plan(DetachScope::Navigation {
                destination: Some(ready.started.session.thread_id),
            })
            .await;
        operation.intent = NavigationIntent::Resume(ResumeStage::Detaching {
            ready,
            plan: plan.clone(),
        });
        self.spawn_resume_effect(
            &mut operation,
            server.request_handle(),
            ResumeEffect::Detach(plan),
        );
        self.server_operations.navigation.current = Some(operation);
    }

    pub(super) async fn finish_resume_navigation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        matching: bool,
        receipt: ResumeReceipt,
    ) -> Result<AppRunControl> {
        let same_epoch = operation.epoch == self.server_operations.epoch;
        let NavigationIntent::Resume(stage) =
            std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("resume receipt without resume stage");
        };
        let ResumeReceipt { handle, outcome } = receipt;
        match (stage, outcome) {
            (ResumeStage::TrustedConfig(mut context), ResumeOutcome::Rebuilt(result))
                if matching =>
            {
                match result {
                    Ok(projected) => {
                        let (config, settings) = *projected;
                        context.config = config;
                        context.settings = settings;
                    }
                    Err(error) => {
                        self.add_agents_overview_error(format!(
                            "Failed to reload trusted folder settings: {error}"
                        ));
                        return Ok(AppRunControl::Continue);
                    }
                }
                if matches!(self.app_server_target, AppServerTarget::Embedded) {
                    let cwd = context.config.cwd.to_path_buf();
                    operation.intent = NavigationIntent::Resume(ResumeStage::Hooks(context));
                    self.spawn_resume_effect(&mut operation, handle, ResumeEffect::Hooks(cwd));
                } else {
                    self.prepare_resume_acquisition(server, &mut operation, context);
                }
                self.server_operations.navigation.current = Some(operation);
            }
            (ResumeStage::Rebuilding { target, source }, ResumeOutcome::Rebuilt(result))
                if matching =>
            {
                return Ok(
                    self.finish_rebuilt_resume_config(server, operation, target, source, result)
                );
            }
            (
                ResumeStage::OverviewConfig { thread_id, after },
                ResumeOutcome::Refreshed(result),
            ) if matching => {
                self.apply_navigation_config_refresh(result, "attaching a task");
                operation.intent =
                    NavigationIntent::Resume(ResumeStage::OverviewMetadata { thread_id, after });
                self.spawn_resume_effect(&mut operation, handle, ResumeEffect::Thread(thread_id));
                self.server_operations.navigation.current = Some(operation);
            }
            (ResumeStage::OverviewMetadata { thread_id, after }, ResumeOutcome::Thread(result))
                if matching =>
            {
                match result {
                    Ok(thread) => {
                        return self
                            .begin_overview_attachment(
                                tui, server, operation, thread_id, thread, after,
                            )
                            .await;
                    }
                    Err(error) => self.add_agents_overview_error(format!(
                        "Failed to read task settings: {error}"
                    )),
                }
            }
            (ResumeStage::RestoringBlank(context), ResumeOutcome::Cached(started)) if matching => {
                self.detach_for_resume(
                    server,
                    operation,
                    Box::new(ResumeReady {
                        context: *context,
                        started,
                        presentation: ResumePresentation::CachedBlank,
                    }),
                )
                .await;
            }
            (ResumeStage::RestoringBlank(_), ResumeOutcome::Cached(_)) => {}
            (ResumeStage::Config(target), ResumeOutcome::Refreshed(result)) if matching => {
                self.apply_navigation_config_refresh(result, "resuming a thread");
                let basis = match self.capture_resume_cwd_basis(server) {
                    Ok(basis) => basis,
                    Err(control) => return Ok(control),
                };
                let needs_history = !self.app_server_target.uses_remote_workspace()
                    && !matches!(basis.mode, Some(ResumeCwdMode::Current));
                if needs_history {
                    let thread_id = target.thread_id;
                    operation.intent = NavigationIntent::Resume(ResumeStage::Cwd {
                        target,
                        basis,
                        source: ResumeSource::Explicit,
                    });
                    self.spawn_resume_effect(
                        &mut operation,
                        handle,
                        ResumeEffect::Thread(thread_id),
                    );
                    self.server_operations.navigation.current = Some(operation);
                } else {
                    return self
                        .finish_resume_cwd(
                            tui,
                            server,
                            operation,
                            target,
                            basis,
                            None,
                            ResumeSource::Explicit,
                        )
                        .await;
                }
            }
            (
                ResumeStage::Cwd {
                    target,
                    basis,
                    source,
                },
                ResumeOutcome::Thread(result),
            ) if matching => {
                let history_cwd = match result {
                    Ok(thread) => Some(thread.cwd.to_path_buf()),
                    Err(error) => {
                        tracing::warn!(%error, "failed to read resume working directory");
                        None
                    }
                };
                return self
                    .finish_resume_cwd(tui, server, operation, target, basis, history_cwd, source)
                    .await;
            }
            (ResumeStage::TrustMetadata(context), ResumeOutcome::Thread(result)) if matching => {
                match result {
                    Ok(thread) => {
                        return Ok(self.begin_resume_trust(
                            server,
                            operation,
                            context,
                            Some(&thread),
                        ));
                    }
                    Err(error) => self.add_agents_overview_error(format!(
                        "Unable to check resumed folder: {error:#}"
                    )),
                }
            }
            (ResumeStage::Trust { context, mut plan }, ResumeOutcome::Project(result))
                if matching =>
            {
                let project = match result.and_then(|project| plan.accept_project(project)) {
                    Ok(project) => project,
                    Err(error) => {
                        self.add_agents_overview_error(format!(
                            "Unable to check folder trust: {error}"
                        ));
                        return Ok(AppRunControl::Continue);
                    }
                };
                if let Some(project) = project {
                    let consent = match plan.choose(tui, handle.clone(), project).await {
                        Ok(DirectoryTrustDecision::Confirmed(consent)) => consent,
                        Ok(DirectoryTrustDecision::Consent(consent)) => {
                            self.transfer_navigation_trust(
                                server,
                                operation,
                                ResumeTrustConsent::Directory {
                                    consent,
                                    context,
                                    plan,
                                },
                            );
                            return Ok(AppRunControl::Continue);
                        }
                        Err(error) => {
                            self.add_agents_overview_error(format!(
                                "Unable to check folder trust: {error}"
                            ));
                            return Ok(AppRunControl::Continue);
                        }
                    };
                    if plan.note_consent(consent).is_some() {
                        if matches!(self.app_server_target, AppServerTarget::Embedded) {
                            return Ok(AppRunControl::Exit(ExitReason::UserRequested));
                        }
                        if self
                            .chat_widget
                            .selected_index_for_present_view(AGENTS_OVERVIEW_VIEW_ID)
                            .is_none()
                        {
                            self.open_agents_overview(server);
                        }
                        return Ok(AppRunControl::Continue);
                    }
                    if let Some(thread_id) = plan.recheck_thread() {
                        operation.intent =
                            NavigationIntent::Resume(ResumeStage::Trust { context, plan });
                        self.spawn_resume_effect(
                            &mut operation,
                            handle,
                            ResumeEffect::Recheck(thread_id),
                        );
                        self.server_operations.navigation.current = Some(operation);
                        return Ok(AppRunControl::Continue);
                    }
                }
                self.advance_resume_trust(server, &mut operation, context, plan);
                if matches!(operation.intent, NavigationIntent::Resume(_)) {
                    self.server_operations.navigation.current = Some(operation);
                }
            }
            (ResumeStage::Trust { context, mut plan }, ResumeOutcome::Rechecked(result))
                if matching =>
            {
                match result {
                    Ok(thread) => plan.note_latest_cwd(thread.cwd.to_path_buf()),
                    Err(error) => {
                        self.add_agents_overview_error(format!(
                            "Unable to check resumed folder: {error}"
                        ));
                        return Ok(AppRunControl::Continue);
                    }
                }
                self.advance_resume_trust(server, &mut operation, context, plan);
                if matches!(operation.intent, NavigationIntent::Resume(_)) {
                    self.server_operations.navigation.current = Some(operation);
                }
            }
            (ResumeStage::Hooks(context), ResumeOutcome::Hooks(result)) if matching => {
                match result {
                    Ok(hooks) => {
                        self.review_resume_hooks(tui, server, operation, context, hooks)
                            .await?;
                    }
                    Err(error) => self
                        .add_agents_overview_error(format!("Unable to load folder hooks: {error}")),
                }
            }
            (ResumeStage::Acquiring(context), ResumeOutcome::Acquired(prepared)) => {
                let PreparedSession {
                    result,
                    custody,
                    observations,
                } = prepared;
                let valid_origin = server.accepts_session_observations(&observations);
                let matching = matching && valid_origin;
                let result = if valid_origin {
                    result
                } else {
                    Err(color_eyre::eyre::eyre!(
                        "prepared session belongs to a different backend"
                    ))
                };
                if same_epoch && valid_origin {
                    server.apply_session_observations(observations)?;
                }
                match result {
                    Ok(started) if matching => {
                        self.detach_for_resume(
                            server,
                            operation,
                            Box::new(ResumeReady {
                                context: *context,
                                started,
                                presentation: ResumePresentation::Writable,
                            }),
                        )
                        .await;
                    }
                    result => {
                        if matching
                            && !(matches!(custody, SubscriptionCustody::None)
                                && result
                                    .as_ref()
                                    .is_err_and(crate::app_server_session::is_active_writer_error))
                            && let Err(error) = &result
                        {
                            self.add_agents_overview_error(format!(
                                "Failed to resume session from {}: {error:#}",
                                context.target.display_label()
                            ));
                        }
                        match custody {
                            SubscriptionCustody::Subscribed(thread_id) => self
                                .start_navigation_cleanup(
                                    operation,
                                    handle,
                                    thread_id,
                                    Vec::new(),
                                    CleanupContinuation::Done,
                                    false,
                                ),
                            SubscriptionCustody::Uncertain(thread_id) if same_epoch => {
                                self.chat_widget.add_error_message("Conversation attachment outcome is unknown. No resume was retried; close and reopen this TUI to resynchronize. The earlier request may still complete.".into());
                                self.retain_navigation_uncertainty(
                                    operation,
                                    vec![thread_id],
                                    false,
                                );
                            }
                            SubscriptionCustody::None
                                if matching
                                    && result.as_ref().is_err_and(|error| {
                                        crate::app_server_session::is_active_writer_error(error)
                                            || crate::app_server_session::model_provider_error(
                                                error,
                                            )
                                            .is_some()
                                    }) =>
                            {
                                let plan = server.prepare_thread_view(
                                    RequestPolicy::Interactive,
                                    &context.config,
                                    &context.settings,
                                    context.target.thread_id,
                                );
                                let cause = result
                                    .as_ref()
                                    .err()
                                    .and_then(crate::app_server_session::model_provider_error)
                                    .map_or(ReadOnlyCause::ExternalWriter, ReadOnlyCause::Provider);
                                operation.intent = NavigationIntent::Resume(ResumeStage::Viewing {
                                    context,
                                    cause,
                                });
                                self.spawn_resume_effect(
                                    &mut operation,
                                    handle,
                                    ResumeEffect::View(Box::new(plan)),
                                );
                                self.server_operations.navigation.current = Some(operation);
                            }
                            _ => {}
                        }
                    }
                }
            }
            (ResumeStage::Viewing { context, cause }, ResumeOutcome::Viewed(prepared))
                if matching =>
            {
                if !server.accepts_session_observations(&prepared.observations) {
                    self.add_agents_overview_error(
                        "prepared thread view belongs to a different backend".into(),
                    );
                    return Ok(AppRunControl::Continue);
                }
                server.apply_session_observations(prepared.observations)?;
                match prepared.result {
                    Ok((started, notice)) => {
                        self.detach_for_resume(
                            server,
                            operation,
                            Box::new(ResumeReady {
                                context: *context,
                                started,
                                presentation: ResumePresentation::ReadOnly { notice, cause },
                            }),
                        )
                        .await;
                    }
                    Err(error) => self.add_agents_overview_error(format!(
                        "Failed to view saved thread: {error:#}"
                    )),
                }
            }
            (ResumeStage::Detaching { ready, plan }, ResumeOutcome::Detached(receipt)) => {
                let fences = match &receipt {
                    detach::DetachReceipt::Complete(results) => results
                        .iter()
                        .filter(|result| result.result.is_err())
                        .map(|result| result.thread_id)
                        .collect::<Vec<_>>(),
                    detach::DetachReceipt::BudgetExpired => {
                        plan.targets.iter().map(|target| target.thread_id).collect()
                    }
                };
                if same_epoch && let detach::DetachReceipt::Complete(results) = receipt {
                    self.finish_detach_plan(
                        &plan,
                        detach::DetachReceipt::Complete(
                            results
                                .into_iter()
                                .filter(|result| result.result.is_ok())
                                .collect(),
                        ),
                    )
                    .await;
                }
                if !matching || !fences.is_empty() {
                    if same_epoch && !fences.is_empty() {
                        self.chat_widget.add_error_message("Navigation detachment was not confirmed. Close and reopen this TUI to resynchronize; no detach was retried.".into());
                    }
                    match ready.presentation {
                        ResumePresentation::Writable => self.start_navigation_cleanup(
                            operation,
                            handle,
                            ready.started.session.thread_id,
                            if same_epoch { fences } else { Vec::new() },
                            CleanupContinuation::Done,
                            false,
                        ),
                        ResumePresentation::ReadOnly { .. } | ResumePresentation::CachedBlank
                            if same_epoch && !fences.is_empty() =>
                        {
                            self.retain_navigation_uncertainty(operation, fences, false);
                        }
                        ResumePresentation::ReadOnly { .. } | ResumePresentation::CachedBlank => {}
                    }
                } else {
                    Box::pin(self.commit_resumed_navigation(tui, server, operation, *ready))
                        .await?;
                }
            }
            (
                ResumeStage::Following {
                    thread_id,
                    presentation,
                    after,
                },
                ResumeOutcome::Loaded(result),
            ) if matching => {
                if self.current_displayed_thread_id() == Some(thread_id) {
                    match result {
                        Ok(observations) => {
                            self.apply_loaded_subagent_observations(thread_id, observations);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "failed to list loaded threads for subagent backfill");
                        }
                    }
                    if matches!(
                        presentation,
                        ResumePresentation::Writable | ResumePresentation::CachedBlank
                    ) {
                        self.replay_agents_overview_requests(server, thread_id)
                            .await;
                        if !matches!(after, SelectionAfter::Fork)
                            && !self.chat_widget.has_misalignment_policy_violation()
                        {
                            operation.intent =
                                NavigationIntent::Resume(ResumeStage::Goal(thread_id));
                            self.spawn_resume_effect(
                                &mut operation,
                                handle,
                                ResumeEffect::Goal(thread_id),
                            );
                            self.server_operations.navigation.current = Some(operation);
                        }
                    }
                    self.finish_selection_after(tui, server, after).await?;
                }
            }
            (ResumeStage::Goal(thread_id), ResumeOutcome::Goal(result)) if matching => {
                if self.current_displayed_thread_id() == Some(thread_id)
                    && !self.chat_widget.has_misalignment_policy_violation()
                {
                    match result {
                        Ok(response) => {
                            if let Some(goal) = response.goal
                                && matches!(
                                    goal.status,
                                    api::ThreadGoalStatus::Paused
                                        | api::ThreadGoalStatus::Blocked
                                        | api::ThreadGoalStatus::UsageLimited
                                )
                            {
                                self.chat_widget
                                    .show_resume_paused_goal_prompt(thread_id, goal.objective);
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "failed to read thread goal after resume");
                        }
                    }
                }
            }
            (stage, ResumeOutcome::Terminated) => {
                if matching && let Some(source) = stage.pending_fork_source() {
                    self.cancel_fork_projection(source);
                }
                self.add_agents_overview_error("Conversation preparation terminated unexpectedly. No request was retried; close and reopen this TUI to resynchronize.".into());
                match stage {
                    ResumeStage::Acquiring(context) if same_epoch => self
                        .retain_navigation_uncertainty(
                            operation,
                            vec![context.target.thread_id],
                            false,
                        ),
                    ResumeStage::Detaching { ready, plan } => {
                        let fences = if same_epoch {
                            plan.targets.iter().map(|target| target.thread_id).collect()
                        } else {
                            Vec::new()
                        };
                        if matches!(ready.presentation, ResumePresentation::Writable) {
                            self.start_navigation_cleanup(
                                operation,
                                handle,
                                ready.started.session.thread_id,
                                fences,
                                CleanupContinuation::Done,
                                false,
                            );
                        } else if same_epoch && !fences.is_empty() {
                            self.retain_navigation_uncertainty(operation, fences, false);
                        }
                    }
                    _ => {}
                }
            }
            (stage, _) if !matching && stage.is_read_only() => {}
            _ => unreachable!("resume stage received a different stage's receipt"),
        }
        Ok(AppRunControl::Continue)
    }

    fn start_resume_config_rebuild(
        &mut self,
        server: &AppServerSession,
        mut operation: PendingNavigation,
        target: SessionTarget,
        source: ResumeSource,
        plan: Box<navigation_config::ConfigRebuild>,
    ) {
        operation.intent = NavigationIntent::Resume(ResumeStage::Rebuilding { target, source });
        self.spawn_resume_effect(
            &mut operation,
            server.request_handle(),
            ResumeEffect::Rebuild(plan),
        );
        self.server_operations.navigation.current = Some(operation);
    }

    async fn finish_resume_cwd(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        operation: PendingNavigation,
        target: SessionTarget,
        basis: ResumeCwdBasis,
        history_cwd: Option<PathBuf>,
        source: ResumeSource,
    ) -> Result<AppRunControl> {
        let plan = match self
            .project_resume_config(tui, &target, basis, history_cwd)
            .await
        {
            Ok(plan) => plan,
            Err(control) => return Ok(control),
        };
        self.start_resume_config_rebuild(server, operation, target, source, plan);
        Ok(AppRunControl::Continue)
    }

    fn finish_rebuilt_resume_config(
        &mut self,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        target: SessionTarget,
        source: ResumeSource,
        result: Result<Box<(Config, LocalSettings)>>,
    ) -> AppRunControl {
        let (config, settings) = match result {
            Ok(result) => *result,
            Err(error) => {
                self.add_agents_overview_error(format!(
                    "Failed to rebuild configuration for resume: {error}"
                ));
                return AppRunControl::Continue;
            }
        };
        let loaded = match &source {
            ResumeSource::Overview { thread, .. }
                if !matches!(thread.status, api::ThreadStatus::NotLoaded) =>
            {
                Some(thread.clone())
            }
            _ => None,
        };
        let app_server_target = self.app_server_target.clone();
        if loaded.is_none()
            && self.reject_remote_resume_permission_override(&config, &app_server_target)
        {
            return AppRunControl::Continue;
        }
        let purpose = ResumePurpose::from_source(source, &config);
        let context = Box::new(ResumeContext {
            target,
            config,
            settings,
            purpose,
        });
        if let Some(thread) = loaded {
            return self.begin_resume_trust(server, operation, context, Some(&thread));
        }
        if matches!(self.app_server_target, AppServerTarget::LocalDaemon { .. }) {
            let thread_id = context.target.thread_id;
            operation.intent = NavigationIntent::Resume(ResumeStage::TrustMetadata(context));
            self.spawn_resume_effect(
                &mut operation,
                server.request_handle(),
                ResumeEffect::Thread(thread_id),
            );
            self.server_operations.navigation.current = Some(operation);
            AppRunControl::Continue
        } else {
            self.begin_resume_trust(server, operation, context, None)
        }
    }

    async fn commit_resumed_navigation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        ready: ResumeReady,
    ) -> Result<()> {
        if matches!(ready.context.purpose, ResumePurpose::Overview { .. }) {
            return Box::pin(self.commit_overview_attachment(tui, server, operation, ready)).await;
        }
        let thread_id = ready.started.session.thread_id;
        let has_authoritative_mode = ready.started.session.collaboration_mode.is_some();
        let running = ready
            .started
            .turns
            .last()
            .is_some_and(|turn| turn.status == TurnStatus::InProgress);
        let read_only = matches!(ready.presentation, ResumePresentation::ReadOnly { .. });
        let retained_input = ((self.chat_widget.is_external_writer_view()
            || self.thread_unavailable(thread_id))
            && self.chat_widget.thread_id() == Some(thread_id))
        .then(|| self.chat_widget.capture_thread_input_state())
        .flatten();
        self.local_settings = ready.context.settings;
        self.config = ready.context.config;
        self.refresh_server_version_overview_notice(CODEX_CLI_VERSION);
        tui.set_notification_settings(&self.local_settings.tui.notification_settings);
        self.file_search
            .update_search_dir(self.config.cwd.to_path_buf());
        if let Err(error) = Box::pin(self.replace_chat_widget_with_app_server_thread(
            tui,
            ready.started,
            ThreadAttachPresentation::SessionLineage,
            None,
        ))
        .await
        {
            self.add_agents_overview_error(format!(
                "Failed to attach to resumed app-server thread: {error}"
            ));
            if !read_only && self.chat_widget.thread_id() != Some(thread_id) {
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
        if !read_only
            && self
                .runtime_permission_profile_override
                .as_ref()
                .is_some_and(|profile| {
                    profile.turn_override == RuntimePermissionProfileTurnOverride::LegacySandbox
                        && profile.matches_config(self.chat_widget.config_ref())
                })
        {
            self.runtime_permission_profile_override =
                Some(RuntimePermissionProfileOverride::from_restored_config(
                    self.chat_widget.config_ref(),
                ));
        }
        // Resume admission does not switch widgets. Restore the destination only after
        // installation succeeds, keeping its saved input available on failure.
        if let Some(mut input) =
            retained_input.or(self.agents_overview.input_states.remove(&thread_id))
        {
            ready.presentation.refresh_provider_error(&mut input);
            let pending_settings = matches!(ready.presentation, ResumePresentation::CachedBlank)
                .then(|| input.pending_thread_settings.take())
                .flatten();
            input.agent_turn_running = running;
            input.task_running = running;
            let accepted_mode =
                has_authoritative_mode.then(|| self.chat_widget.effective_collaboration_mode());
            self.chat_widget.restore_bound_thread_input_state(
                Some(input),
                ThreadInputStateRestoreMode {
                    preserve_in_flight_turn: running && !read_only,
                },
                if matches!(ready.presentation, ResumePresentation::CachedBlank) {
                    crate::chatwidget::ThreadInputSettingsAuthority::Retained
                } else {
                    crate::chatwidget::ThreadInputSettingsAuthority::Session { accepted_mode }
                },
            );
            if let Some(settings) = pending_settings {
                self.chat_widget.on_thread_settings_updated(settings);
            }
        }
        if !read_only {
            self.chat_widget.maybe_send_next_queued_input();
        }
        if let ResumePresentation::ReadOnly { notice, cause } = &ready.presentation {
            self.apply_resumed_read_only(thread_id, *notice, cause);
        }
        if matches!(
            self.runtime_approval_policy_override,
            Some(RuntimeApprovalPolicyOverride::Restored(_))
        ) {
            self.runtime_approval_policy_override = None;
        }
        if self
            .runtime_permission_profile_override
            .as_ref()
            .is_some_and(|profile| {
                profile.turn_override == RuntimePermissionProfileTurnOverride::Preserve
            })
        {
            self.runtime_permission_profile_override = None;
        }
        operation.intent = NavigationIntent::Resume(ResumeStage::Following {
            thread_id,
            presentation: ready.presentation,
            after: SelectionAfter::SelectOnly,
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

impl App {
    fn apply_resumed_read_only(
        &mut self,
        thread_id: ThreadId,
        notice: Option<&'static str>,
        cause: &ReadOnlyCause,
    ) {
        match cause {
            ReadOnlyCause::ExternalWriter => {
                self.ensure_thread_channel(thread_id).mark_external_writer();
                self.chat_widget.show_external_writer_thread();
            }
            ReadOnlyCause::Provider(error) => {
                self.ensure_thread_channel(thread_id).mark_replay_only();
                self.chat_widget.show_provider_error(error.clone(), None);
            }
        }
        if let Some(notice) = notice {
            self.chat_widget.add_info_message(notice.into(), None);
        }
    }
}
