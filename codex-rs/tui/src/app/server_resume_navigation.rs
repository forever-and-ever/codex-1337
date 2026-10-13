use super::trust::TrustWrite;
use super::*;
use crate::app::resume_config::ResumeCwdBasis;
use crate::app::session_lifecycle::ThreadAttachPresentation;
use crate::app_server_session::session_preparation::{
    self as preparation, PreparedSession, PreparedView, RequestPolicy, ResumePreparation,
    SubscriptionCustody, ViewPreparation,
};
use crate::chatwidget::ThreadInputStateRestoreMode;
use crate::config_update::{
    ProjectTrustHost, RemoteProjectTrust, read_remote_project_trust_with_policy,
};
use crate::local_settings::LocalSettings;
use crate::onboarding::onboarding_screen::DirectoryTrustDecision;
use crate::onboarding::onboarding_screen::DirectoryTrustPreparation;
use crate::startup_hooks_review::load_startup_hooks_review_entry;
use crate::startup_hooks_review::{
    HooksReviewDecision, choose_startup_hooks_review, hook_trust_updates,
};
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;
use codex_config::types::ResumeCwdMode;

#[derive(Clone)]
pub(super) struct ResumeContext {
    target: SessionTarget,
    config: Config,
    settings: LocalSettings,
}

#[derive(Clone)]
pub(super) struct ResumeTrustContinuation {
    pub(super) generation: u64,
    pub(super) next: ResumeTrustNext,
}
#[derive(Clone)]
pub(super) enum ResumeTrustNext {
    Directory {
        context: Box<ResumeContext>,
        plan: DirectoryTrustPreparation,
    },
    Hooks(Box<ResumeContext>),
}

impl ResumeTrustNext {
    pub(super) fn hook_scope(&self) -> String {
        let Self::Hooks(context) = self else {
            unreachable!("hook trust must retain its hook review continuation");
        };
        format!("hooks in {}", context.config.cwd.to_path_buf().display())
    }
}

pub(super) struct ResumeReady {
    context: ResumeContext,
    started: crate::app_server_session::AppServerStartedThread,
    presentation: ResumePresentation,
}

pub(super) enum ResumePresentation {
    Writable,
    ReadOnly(Option<&'static str>),
}

pub(super) enum ResumeStage {
    Admitted(SessionTarget),
    Config(SessionTarget),
    Cwd {
        target: SessionTarget,
        basis: ResumeCwdBasis,
    },
    TrustMetadata(Box<ResumeContext>),
    Trust {
        context: Box<ResumeContext>,
        plan: DirectoryTrustPreparation,
    },
    Hooks(Box<ResumeContext>),
    Acquiring(Box<ResumeContext>),
    Viewing(Box<ResumeContext>),
    Detaching {
        ready: Box<ResumeReady>,
        plan: detach::DetachPlan,
    },
    Following {
        thread_id: ThreadId,
        presentation: ResumePresentation,
    },
    Goal(ThreadId),
}

impl ResumeStage {
    pub(super) fn thread_id(&self) -> ThreadId {
        match self {
            Self::Admitted(target) | Self::Config(target) | Self::Cwd { target, .. } => {
                target.thread_id
            }
            Self::TrustMetadata(context)
            | Self::Trust { context, .. }
            | Self::Hooks(context)
            | Self::Acquiring(context)
            | Self::Viewing(context) => context.target.thread_id,
            Self::Detaching { ready, .. } => ready.started.session.thread_id,
            Self::Following { thread_id, .. } | Self::Goal(thread_id) => *thread_id,
        }
    }

    pub(super) fn is_read_only(&self) -> bool {
        !matches!(self, Self::Acquiring(_) | Self::Detaching { .. })
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
    Begin,
    Thread(Result<api::Thread>),
    Project(Result<Option<RemoteProjectTrust>>),
    Rechecked(Result<api::Thread>),
    Hooks(Result<api::HooksListEntry, String>),
    Acquired(PreparedSession),
    Viewed(PreparedView),
    Detached(detach::DetachReceipt),
    Loaded(Result<Vec<(ThreadId, Result<api::Thread>)>>),
    Goal(Result<api::ThreadGoalGetResponse>),
    Terminated,
}

enum ResumeEffect {
    Begin,
    Thread(ThreadId),
    Project {
        cwd: PathBuf,
        host: ProjectTrustHost,
    },
    Recheck(ThreadId),
    Hooks(PathBuf),
    Acquire(ResumePreparation),
    View(ViewPreparation),
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
        let NavigationIntent::Resume(ResumeStage::Admitted(target)) =
            std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("resume dispatch requires admitted target");
        };
        operation.intent = NavigationIntent::Resume(ResumeStage::Config(target));
        self.spawn_resume_effect(operation, server.request_handle(), ResumeEffect::Begin);
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
            let outcome = std::panic::AssertUnwindSafe(async {
                match effect {
                    ResumeEffect::Begin => ResumeOutcome::Begin,
                    ResumeEffect::Thread(thread_id) => ResumeOutcome::Thread(
                        preparation::read_thread(RequestPolicy::Interactive, &handle, thread_id)
                            .await,
                    ),
                    ResumeEffect::Recheck(thread_id) => ResumeOutcome::Rechecked(
                        preparation::read_thread(RequestPolicy::Interactive, &handle, thread_id)
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
                    ResumeEffect::Acquire(plan) => ResumeOutcome::Acquired(plan.run().await),
                    ResumeEffect::View(plan) => ResumeOutcome::Viewed(plan.run().await),
                    ResumeEffect::Detach(plan) => ResumeOutcome::Detached(
                        match tokio::time::timeout(ACK_TIMEOUT, detach::send(&handle, id, &plan))
                            .await
                        {
                            Ok(results) => detach::DetachReceipt::Complete(results),
                            Err(_) => detach::DetachReceipt::BudgetExpired,
                        },
                    ),
                    ResumeEffect::Loaded(root) => ResumeOutcome::Loaded(
                        preparation::loaded_observations(RequestPolicy::Interactive, &handle, root)
                            .await,
                    ),
                    ResumeEffect::Goal(thread_id) => ResumeOutcome::Goal(
                        preparation::request(
                            RequestPolicy::Interactive,
                            &handle,
                            api::ClientRequest::ThreadGoalGet {
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
            .catch_unwind()
            .await
            .unwrap_or(ResumeOutcome::Terminated);
            let _ = completions.send(ServerCompletion::Navigation(NavigationCompletion {
                id,
                order,
                epoch,
                generation,
                receipt: NavigationReceipt::Resumed(ResumeReceipt { handle, outcome }),
            }));
        })));
    }

    async fn begin_resume_trust(
        &mut self,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        context: Box<ResumeContext>,
        resumed_thread: Option<&api::Thread>,
    ) -> Result<AppRunControl> {
        let cwd = if self.app_server_target.uses_remote_workspace() {
            let Some(cwd) = server.remote_cwd_override() else {
                self.acquire_resumed_thread(server, operation, context);
                return Ok(AppRunControl::Continue);
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
                self.add_session_picker_error(format!("Unable to check folder trust: {error}"));
                return Ok(AppRunControl::Continue);
            }
        };
        self.advance_resume_trust(server, &mut operation, context, plan)
            .await?;
        if matches!(operation.intent, NavigationIntent::Resume(_)) {
            self.server_operations.navigation.current = Some(operation);
        }
        Ok(AppRunControl::Continue)
    }

    async fn advance_resume_trust(
        &mut self,
        server: &mut AppServerSession,
        operation: &mut PendingNavigation,
        mut context: Box<ResumeContext>,
        mut plan: DirectoryTrustPreparation,
    ) -> Result<()> {
        if let Some(cwd) = plan.next_cwd() {
            let host = plan.host();
            operation.intent = NavigationIntent::Resume(ResumeStage::Trust { context, plan });
            self.spawn_resume_effect(
                operation,
                server.request_handle(),
                ResumeEffect::Project { cwd, host },
            );
            return Ok(());
        }
        let consent = plan.finish();
        if !server.uses_remote_workspace() && consent.directory_trust_persisted {
            match self
                .rebuild_config_for_cwd(context.config.cwd.to_path_buf())
                .await
            {
                Ok(config) => {
                    context.settings = self.local_settings.reloaded(&config);
                    context.config = config;
                }
                Err(error) => {
                    self.add_session_picker_error(format!(
                        "Failed to reload trusted folder settings: {error}"
                    ));
                    return Ok(());
                }
            }
            if matches!(self.app_server_target, AppServerTarget::Embedded) {
                let cwd = context.config.cwd.to_path_buf();
                operation.intent = NavigationIntent::Resume(ResumeStage::Hooks(context));
                self.spawn_resume_effect(
                    operation,
                    server.request_handle(),
                    ResumeEffect::Hooks(cwd),
                );
                return Ok(());
            }
        }
        self.prepare_resume_acquisition(server, operation, context);
        Ok(())
    }

    async fn review_resume_hooks(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        operation: PendingNavigation,
        context: Box<ResumeContext>,
        hooks: api::HooksListEntry,
    ) -> Result<()> {
        let decision = match choose_startup_hooks_review(tui, &context.config, hooks, None).await {
            Ok(decision) => decision,
            Err(error) => {
                self.add_session_picker_error(format!("Unable to review folder hooks: {error}"));
                return Ok(());
            }
        };
        match decision {
            HooksReviewDecision::Continue => {
                self.acquire_resumed_thread(server, operation, context)
            }
            HooksReviewDecision::OpenHooksBrowser(hooks) => {
                self.chat_widget.open_hooks_browser(hooks)
            }
            HooksReviewDecision::TrustAll(hooks) => self.transfer_navigation_trust(
                server,
                operation,
                TrustWrite::Hooks(hook_trust_updates(&hooks)),
                ResumeTrustNext::Hooks(context),
            ),
        }
        Ok(())
    }

    pub(super) async fn continue_resume_trust_saved(
        &mut self,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        next: ResumeTrustNext,
    ) -> Result<()> {
        match next {
            ResumeTrustNext::Hooks(context) => {
                self.acquire_resumed_thread(server, operation, context)
            }
            ResumeTrustNext::Directory { context, mut plan } => {
                let exit =
                    plan.note_consent(crate::onboarding::onboarding_screen::OnboardingResult {
                        directory_trust_persisted: true,
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
                    self.advance_resume_trust(server, &mut operation, context, plan)
                        .await?;
                    if matches!(operation.intent, NavigationIntent::Resume(_)) {
                        self.server_operations.navigation.current = Some(operation);
                    }
                }
            }
        }
        Ok(())
    }

    fn prepare_resume_acquisition(
        &mut self,
        server: &mut AppServerSession,
        operation: &mut PendingNavigation,
        mut context: Box<ResumeContext>,
    ) {
        if let Err(error) = self.apply_runtime_policy_overrides(
            &mut context.config,
            RuntimePolicyOverrideScope::ExplicitOnly,
        ) {
            self.add_session_picker_error(format!("{error:#}"));
            return;
        }
        if let Some(mode) = context.target.history_mode {
            server.remember_thread_history_mode(context.target.thread_id, mode);
        }
        let plan = server.prepare_resume_thread(
            RequestPolicy::Interactive,
            &context.settings,
            context.config.clone(),
            context.target.thread_id,
            self.resume_model_settings(),
            self.resume_permission_overrides(&context.config),
        );
        operation.intent = NavigationIntent::Resume(ResumeStage::Acquiring(context));
        self.spawn_resume_effect(
            operation,
            server.request_handle(),
            ResumeEffect::Acquire(plan),
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
        let plan = self
            .prepare_detach_plan(detach::DetachScope::Navigation {
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
            (ResumeStage::Config(target), ResumeOutcome::Begin) if matching => {
                self.refresh_in_memory_config_from_disk_best_effort("resuming a thread")
                    .await;
                let basis = match self.capture_resume_cwd_basis(server) {
                    Ok(basis) => basis,
                    Err(control) => return Ok(control),
                };
                let needs_history = !self.app_server_target.uses_remote_workspace()
                    && !matches!(basis.mode, Some(ResumeCwdMode::Current));
                if needs_history {
                    let thread_id = target.thread_id;
                    operation.intent = NavigationIntent::Resume(ResumeStage::Cwd { target, basis });
                    self.spawn_resume_effect(
                        &mut operation,
                        handle,
                        ResumeEffect::Thread(thread_id),
                    );
                    self.server_operations.navigation.current = Some(operation);
                } else {
                    return self
                        .finish_resume_cwd(tui, server, operation, target, basis, None)
                        .await;
                }
            }
            (ResumeStage::Cwd { target, basis }, ResumeOutcome::Thread(result)) if matching => {
                let history_cwd = match result {
                    Ok(thread) => Some(thread.cwd.to_path_buf()),
                    Err(error) => {
                        tracing::warn!(%error, "failed to read resume working directory");
                        None
                    }
                };
                return self
                    .finish_resume_cwd(tui, server, operation, target, basis, history_cwd)
                    .await;
            }
            (ResumeStage::TrustMetadata(context), ResumeOutcome::Thread(result)) if matching => {
                match result {
                    Ok(thread) => {
                        return self
                            .begin_resume_trust(server, operation, context, Some(&thread))
                            .await;
                    }
                    Err(error) => self.add_session_picker_error(format!(
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
                        self.add_session_picker_error(format!(
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
                                TrustWrite::Directory(consent),
                                ResumeTrustNext::Directory { context, plan },
                            );
                            return Ok(AppRunControl::Continue);
                        }
                        Err(error) => {
                            self.add_session_picker_error(format!(
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
                self.advance_resume_trust(server, &mut operation, context, plan)
                    .await?;
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
                        self.add_session_picker_error(format!(
                            "Unable to check resumed folder: {error}"
                        ));
                        return Ok(AppRunControl::Continue);
                    }
                }
                self.advance_resume_trust(server, &mut operation, context, plan)
                    .await?;
                if matches!(operation.intent, NavigationIntent::Resume(_)) {
                    self.server_operations.navigation.current = Some(operation);
                }
            }
            (ResumeStage::Hooks(context), ResumeOutcome::Hooks(result)) if matching => {
                match result {
                    Ok(hooks) => {
                        self.review_resume_hooks(tui, server, operation, context, hooks)
                            .await?
                    }
                    Err(error) => self
                        .add_session_picker_error(format!("Unable to load folder hooks: {error}")),
                }
            }
            (ResumeStage::Acquiring(context), ResumeOutcome::Acquired(prepared)) => {
                let PreparedSession {
                    result,
                    custody,
                    observations,
                } = prepared;
                if same_epoch {
                    server.apply_session_observations(observations);
                }
                if !matching || result.is_err() {
                    if matching
                        && !(matches!(custody, SubscriptionCustody::None)
                            && result
                                .as_ref()
                                .is_err_and(crate::app_server_session::is_active_writer_error))
                        && let Err(error) = &result
                    {
                        self.add_session_picker_error(format!(
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
                            self.retain_navigation_uncertainty(operation, vec![thread_id], false);
                        }
                        SubscriptionCustody::None
                            if matching
                                && result.as_ref().is_err_and(
                                    crate::app_server_session::is_active_writer_error,
                                ) =>
                        {
                            let plan = server.prepare_thread_view(
                                RequestPolicy::Interactive,
                                &context.config,
                                &context.settings,
                                context.target.thread_id,
                            );
                            operation.intent =
                                NavigationIntent::Resume(ResumeStage::Viewing(context));
                            self.spawn_resume_effect(
                                &mut operation,
                                handle,
                                ResumeEffect::View(plan),
                            );
                            self.server_operations.navigation.current = Some(operation);
                        }
                        _ => {}
                    }
                } else {
                    self.detach_for_resume(
                        server,
                        operation,
                        Box::new(ResumeReady {
                            context: *context,
                            started: result.expect("successful resume preparation"),
                            presentation: ResumePresentation::Writable,
                        }),
                    )
                    .await;
                }
            }
            (ResumeStage::Viewing(context), ResumeOutcome::Viewed(prepared)) if matching => {
                server.apply_session_observations(prepared.observations);
                match prepared.result {
                    Ok((started, notice)) => {
                        self.detach_for_resume(
                            server,
                            operation,
                            Box::new(ResumeReady {
                                context: *context,
                                started,
                                presentation: ResumePresentation::ReadOnly(notice),
                            }),
                        )
                        .await
                    }
                    Err(error) => self.add_session_picker_error(format!(
                        "Failed to view thread open elsewhere: {error:#}"
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
                        ResumePresentation::ReadOnly(_) if same_epoch && !fences.is_empty() => {
                            self.retain_navigation_uncertainty(operation, fences, false)
                        }
                        ResumePresentation::ReadOnly(_) => {}
                    }
                } else {
                    self.commit_resumed_navigation(tui, server, operation, *ready)
                        .await?;
                }
            }
            (
                ResumeStage::Following {
                    thread_id,
                    presentation,
                },
                ResumeOutcome::Loaded(result),
            ) if matching => {
                if self.current_displayed_thread_id() == Some(thread_id) {
                    match result {
                        Ok(observations) => {
                            self.apply_loaded_subagent_observations(thread_id, observations);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "failed to list loaded threads for subagent backfill")
                        }
                    }
                    if matches!(presentation, ResumePresentation::Writable) {
                        self.replay_agents_overview_requests(server, thread_id)
                            .await;
                        if !self.chat_widget.has_misalignment_policy_violation() {
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
                            tracing::warn!(%error, "failed to read thread goal after resume")
                        }
                    }
                }
            }
            (stage, ResumeOutcome::Terminated) => {
                self.add_session_picker_error("Conversation preparation terminated unexpectedly. No request was retried; close and reopen this TUI to resynchronize.".into());
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

    async fn finish_resume_cwd(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        target: SessionTarget,
        basis: ResumeCwdBasis,
        history_cwd: Option<PathBuf>,
    ) -> Result<AppRunControl> {
        let (config, settings) = match self
            .project_resume_config(tui, &target, basis, history_cwd)
            .await
        {
            Ok(result) => result,
            Err(control) => return Ok(control),
        };
        let context = Box::new(ResumeContext {
            target,
            config,
            settings,
        });
        if matches!(self.app_server_target, AppServerTarget::LocalDaemon { .. }) {
            let thread_id = context.target.thread_id;
            operation.intent = NavigationIntent::Resume(ResumeStage::TrustMetadata(context));
            self.spawn_resume_effect(
                &mut operation,
                server.request_handle(),
                ResumeEffect::Thread(thread_id),
            );
            self.server_operations.navigation.current = Some(operation);
            Ok(AppRunControl::Continue)
        } else {
            self.begin_resume_trust(server, operation, context, None)
                .await
        }
    }

    async fn commit_resumed_navigation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        ready: ResumeReady,
    ) -> Result<()> {
        let thread_id = ready.started.session.thread_id;
        let running = ready
            .started
            .turns
            .last()
            .is_some_and(|turn| turn.status == TurnStatus::InProgress);
        let read_only = matches!(ready.presentation, ResumePresentation::ReadOnly(_));
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
        if let Err(error) = self
            .replace_chat_widget_with_app_server_thread(
                tui,
                ready.started,
                ThreadAttachPresentation::SessionLineage,
                None,
            )
            .await
        {
            self.add_session_picker_error(format!(
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
        if let Some(mut input) = retained_input {
            input.agent_turn_running = running;
            input.task_running = running;
            self.chat_widget.restore_thread_input_state(
                Some(input),
                ThreadInputStateRestoreMode {
                    preserve_in_flight_turn: running && !read_only,
                },
            );
        }
        if let ResumePresentation::ReadOnly(notice) = &ready.presentation {
            self.ensure_thread_channel(thread_id).mark_external_writer();
            self.chat_widget.show_external_writer_thread();
            if let Some(notice) = notice {
                self.chat_widget.add_info_message((*notice).into(), None);
            }
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
