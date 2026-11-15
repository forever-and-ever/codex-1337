//! Directory replacement keeps the source selected until exact authority and detachment commit.

use super::ACK_TIMEOUT;
use super::AbortOnDropHandle;
use super::AbsolutePathBuf;
use super::App;
use super::AppServerSession;
use super::AppServerStartedThread;
use super::AppServerTarget;
use super::Arc;
use super::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
use super::CleanupContinuation;
use super::ClientRequest;
use super::Config;
use super::DetachScope;
use super::FutureExt;
use super::HooksListEntry;
use super::LaneState;
use super::NavigationCompletion;
use super::NavigationIntent;
use super::NavigationReceipt;
use super::OperationOrigin;
use super::PathBuf;
use super::PendingNavigation;
use super::PermissionAuthority;
use super::PermissionProfileSelection;
use super::Result;
use super::RuntimeApprovalPolicyOverride;
use super::RuntimeKeymap;
use super::RuntimePermissionProfileTurnOverride;
use super::RuntimePolicyOverrideScope;
use super::ServerCompletion;
use super::ThreadId;
use super::ThreadInputState;
use super::Uuid;
use super::detach;
use super::tui;
use crate::app::working_directory::DirectoryAttachment;
use crate::app::working_directory::background_terminals_blocker;
use crate::app_server_session::fork_preparation::ForkPreparation;
use crate::app_server_session::session_preparation::PreparedSession;
use crate::app_server_session::session_preparation::RequestPolicy;
use crate::app_server_session::session_preparation::StartPreparation;
use crate::app_server_session::session_preparation::SubscriptionCustody;
use crate::app_server_session::session_preparation::ThreadStartChoice;
use crate::app_server_session::session_preparation::{self as preparation};
use crate::config_update::EffectiveConfig;
use crate::config_update::read_effective_config_if_supported;
use crate::config_update::read_remote_project_trust_with_policy;
use crate::local_settings::LocalSettings;
use crate::onboarding::onboarding_screen::DirectoryTrustConsent;
use crate::onboarding::onboarding_screen::DirectoryTrustDecision;
use crate::onboarding::onboarding_screen::DirectoryTrustPreparation;
use crate::onboarding::onboarding_screen::OnboardingResult;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

#[derive(Clone, Copy)]
pub(in crate::app) enum DirectoryHistory {
    Blank,
    Fork,
}

#[derive(Clone)]
pub(super) struct DirectoryInput {
    origin: OperationOrigin,
    source_cwd: AbsolutePathBuf,
    cwd: AbsolutePathBuf,
    permissions: PermissionAuthority,
    input: Option<ThreadInputState>,
    latest_turn_id: Option<String>,
    history: DirectoryHistory,
    tracked: Vec<ThreadId>,
}

impl DirectoryInput {
    #[expect(
        clippy::expect_used,
        reason = "Directory replacement is admitted only from a thread origin."
    )]
    fn thread_id(&self) -> ThreadId {
        self.origin
            .thread_id()
            .expect("directory replacement has a source conversation")
    }
}

#[derive(Clone)]
pub(super) struct DirectoryContext {
    input: DirectoryInput,
    config: Config,
}

pub(super) struct DirectoryReady {
    context: DirectoryContext,
    settings: LocalSettings,
    keymap: RuntimeKeymap,
}

pub(super) struct DirectoryReplacement {
    ready: DirectoryReady,
    started: AppServerStartedThread,
}

pub(super) struct DirectoryTrustAdmission {
    pub(super) consent: DirectoryTrustConsent,
    context: DirectoryContext,
    plan: DirectoryTrustPreparation,
}

impl DirectoryTrustAdmission {
    pub(super) fn scope(&self) -> String {
        match &self.consent {
            DirectoryTrustConsent::Path(path) => path.display().to_string(),
            DirectoryTrustConsent::Key(key) => key.clone(),
        }
    }
}

pub(super) enum DirectoryStage {
    Admitted(Box<DirectoryInput>),
    Config(Box<DirectoryInput>),
    BeforeTrust(Box<DirectoryContext>),
    Trust {
        context: Box<DirectoryContext>,
        plan: DirectoryTrustPreparation,
    },
    Defaults(Box<DirectoryContext>),
    Loaded(Box<DirectoryReady>),
    Status(Box<DirectoryReady>),
    Acquiring(Box<DirectoryReady>),
    Detaching {
        replacement: Box<DirectoryReplacement>,
        plan: detach::DetachPlan,
    },
    Hooks(ThreadId),
}

impl DirectoryStage {
    pub(super) fn is_read_only(&self) -> bool {
        !matches!(self, Self::Acquiring(_) | Self::Detaching { .. })
    }

    pub(super) fn thread_id(&self) -> ThreadId {
        match self {
            Self::Admitted(input) | Self::Config(input) => input.thread_id(),
            Self::BeforeTrust(context) | Self::Trust { context, .. } | Self::Defaults(context) => {
                context.input.thread_id()
            }
            Self::Loaded(ready) | Self::Status(ready) | Self::Acquiring(ready) => {
                ready.context.input.thread_id()
            }
            Self::Detaching { replacement, .. } => replacement.ready.context.input.thread_id(),
            Self::Hooks(thread_id) => *thread_id,
        }
    }

    pub(super) fn fences_thread(&self, thread_id: ThreadId) -> bool {
        self.thread_id() == thread_id
            || matches!(self, Self::Detaching { replacement, plan }
            if replacement.started.session.thread_id == thread_id || plan.targets.iter().any(|target| target.thread_id == thread_id))
    }
}

enum Acquisition {
    Start(StartPreparation),
    Fork(ForkPreparation),
}

enum DirectoryEffect {
    Begin,
    Terminals {
        ids: Vec<ThreadId>,
        target: AppServerTarget,
    },
    Project {
        cwd: PathBuf,
        host: crate::config_update::ProjectTrustHost,
    },
    Defaults(PathBuf),
    Loaded(ThreadId),
    Status {
        ids: Vec<ThreadId>,
        target: AppServerTarget,
    },
    Acquire(Box<Acquisition>),
    Detach(detach::DetachPlan),
    Hooks(PathBuf),
}

pub(super) struct DirectoryReceipt {
    handle: AppServerRequestHandle,
    outcome: DirectoryOutcome,
}

impl DirectoryReceipt {
    pub(super) fn requires_owner(&self) -> bool {
        matches!(
            self.outcome,
            DirectoryOutcome::Acquired(_) | DirectoryOutcome::Detached(_)
        )
    }
}

enum DirectoryOutcome {
    Begin,
    Terminals(Result<(), String>),
    Project(Result<Option<crate::config_update::RemoteProjectTrust>>),
    Defaults(Result<Option<EffectiveConfig>, String>),
    Loaded(Result<Vec<(ThreadId, Result<api::Thread>)>>),
    Status(Result<(), String>),
    Acquired(PreparedSession),
    Detached(detach::DetachReceipt),
    Hooks(Result<HooksListEntry, String>),
    Terminated,
}

async fn terminals(
    handle: &AppServerRequestHandle,
    id: Uuid,
    ids: &[ThreadId],
    target: &AppServerTarget,
) -> Result<(), String> {
    for thread_id in ids {
        let result = preparation::request(
            RequestPolicy::Interactive,
            handle,
            ClientRequest::ThreadBackgroundTerminalsList {
                request_id: api::RequestId::String(format!(
                    "tui-directory-{id}-{thread_id}-terminals"
                )),
                params: api::ThreadBackgroundTerminalsListParams {
                    thread_id: thread_id.to_string(),
                    cursor: None,
                    limit: Some(1),
                    scope: api::ThreadBackgroundTerminalsScope::Thread,
                },
            },
        )
        .await;
        if let Some(message) = background_terminals_blocker(result, target) {
            return Err(message.into());
        }
    }
    Ok(())
}

impl App {
    pub(in crate::app) async fn enqueue_directory_change(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        cwd: AbsolutePathBuf,
        selected_profile: Option<PermissionProfileSelection>,
        history: DirectoryHistory,
        tracked: Vec<ThreadId>,
    ) {
        let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
        let latest_turn_id = store.lock().await.latest_turn_id.clone();
        let input = DirectoryInput {
            origin: OperationOrigin::Thread { thread_id, store },
            source_cwd: self.config.cwd.clone(),
            cwd,
            permissions: PermissionAuthority::capture(
                self.chat_widget.config_ref(),
                selected_profile,
            ),
            input: self.chat_widget.capture_thread_input_state(),
            latest_turn_id,
            history,
            tracked,
        };
        self.enqueue_navigation(
            server,
            NavigationIntent::Directory(DirectoryStage::Admitted(Box::new(input))),
        );
    }

    pub(super) fn dispatch_directory_change(
        &self,
        server: &AppServerSession,
        operation: &mut PendingNavigation,
    ) {
        let NavigationIntent::Directory(DirectoryStage::Admitted(input)) =
            std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("directory dispatch requires admitted source authority");
        };
        operation.intent = NavigationIntent::Directory(DirectoryStage::Config(input));
        self.spawn_directory_effect(operation, server.request_handle(), DirectoryEffect::Begin);
    }

    fn spawn_directory_effect(
        &self,
        operation: &mut PendingNavigation,
        handle: AppServerRequestHandle,
        effect: DirectoryEffect,
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
                        DirectoryEffect::Begin => DirectoryOutcome::Begin,
                        DirectoryEffect::Terminals { ids, target } => {
                            DirectoryOutcome::Terminals(terminals(&handle, id, &ids, &target).await)
                        }
                        DirectoryEffect::Project { cwd, host } => DirectoryOutcome::Project(
                            read_remote_project_trust_with_policy(
                                RequestPolicy::Interactive,
                                handle.clone(),
                                &cwd,
                                host,
                            )
                            .await,
                        ),
                        DirectoryEffect::Defaults(cwd) => DirectoryOutcome::Defaults(
                            tokio::time::timeout(
                                ACK_TIMEOUT,
                                read_effective_config_if_supported(handle.clone(), &cwd),
                            )
                            .await
                            .map_err(|_| "Directory defaults read timed out".into())
                            .and_then(|result| result.map_err(|error| error.to_string())),
                        ),
                        DirectoryEffect::Loaded(root) => DirectoryOutcome::Loaded(
                            preparation::loaded_observations(
                                RequestPolicy::Interactive,
                                &handle,
                                root,
                            )
                            .await,
                        ),
                        DirectoryEffect::Status { ids, target } => DirectoryOutcome::Status(
                            async {
                                for thread_id in &ids {
                                    let thread = preparation::read_thread(
                                        RequestPolicy::Interactive,
                                        &handle,
                                        *thread_id,
                                    )
                                    .await
                                    .map_err(|error| {
                                        format!("Cannot check task status: {error}")
                                    })?;
                                    if matches!(thread.status, api::ThreadStatus::Active { .. }) {
                                        return Err("Cannot change: a task is running.".into());
                                    }
                                }
                                terminals(&handle, id, &ids, &target).await
                            }
                            .await,
                        ),
                        DirectoryEffect::Acquire(plan) => match *plan {
                            Acquisition::Start(plan) => {
                                DirectoryOutcome::Acquired(Box::pin(plan.run()).await)
                            }
                            Acquisition::Fork(plan) => {
                                DirectoryOutcome::Acquired(Box::pin(plan.run()).await)
                            }
                        },
                        DirectoryEffect::Detach(mut plan) => DirectoryOutcome::Detached(
                            match tokio::time::timeout(ACK_TIMEOUT, async {
                                let primary = detach::DetachPlan {
                                    targets: vec![plan.targets.remove(0)],
                                    voice: plan.voice.take(),
                                };
                                let mut results = detach::send(&handle, id, &primary).await;
                                if results.iter().all(|result| result.result.is_ok()) {
                                    results.extend(detach::send(&handle, id, &plan).await);
                                }
                                results
                            })
                            .await
                            {
                                Ok(results) => detach::DetachReceipt::Complete(results),
                                Err(_) => detach::DetachReceipt::BudgetExpired,
                            },
                        ),
                        DirectoryEffect::Hooks(cwd) => DirectoryOutcome::Hooks(
                            tokio::time::timeout(
                                ACK_TIMEOUT,
                                crate::startup_hooks_review::load_startup_hooks_review_entry(
                                    handle.clone(),
                                    cwd,
                                ),
                            )
                            .await
                            .map_err(|_| "Folder hooks read timed out".into()),
                        ),
                    }
                })
                .catch_unwind(),
            )
            .await
            .unwrap_or(DirectoryOutcome::Terminated);
            let _ = completions.send(ServerCompletion::Navigation(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::Directory(DirectoryReceipt { handle, outcome }),
                },
            )));
        })));
    }

    async fn directory_authority_matches(
        &mut self,
        input: &DirectoryInput,
        detached: bool,
    ) -> bool {
        let (thread_id, store) = input.origin.thread();
        if self.primary_thread_id != Some(thread_id)
            || self.config.cwd != input.source_cwd
            || self.chat_widget.config_ref().cwd != input.source_cwd
            || !self.chat_widget.can_change_working_directory(thread_id)
            || self.pending_server_profiles.contains_key(&thread_id)
            || !input.permissions.matches(
                self.chat_widget.config_ref(),
                self.agents_overview
                    .requested_permission_profiles
                    .get(&thread_id),
            )
            || !self
                .thread_event_channels
                .get(&thread_id)
                .is_some_and(|channel| Arc::ptr_eq(&channel.store, store))
            || store.lock().await.latest_turn_id != input.latest_turn_id
        {
            return false;
        }
        let mut expected = input.input.clone();
        if detached && let Some(expected) = &mut expected {
            expected.pause_unavailable_thread();
        }
        match (self.chat_widget.capture_thread_input_state(), expected) {
            (Some(actual), Some(expected)) => actual.same_pending_input(&expected),
            (None, None) => true,
            _ => false,
        }
    }

    fn directory_stale(&mut self) {
        self.working_directory_error("Directory change canceled because the original task, draft or permissions changed. No replacement was installed.");
    }

    fn directory_detached_source(&mut self) {
        self.working_directory_error("Directory change canceled after the original task was detached. Its draft is retained; reopen the original task to continue. No replacement was installed.");
    }

    fn directory_cleanup(
        &mut self,
        operation: PendingNavigation,
        handle: AppServerRequestHandle,
        source: ThreadId,
        replacement: ThreadId,
        history: DirectoryHistory,
        fences: Vec<ThreadId>,
    ) {
        if source == replacement {
            if operation.epoch == self.server_operations.epoch && !fences.is_empty() {
                self.retain_navigation_uncertainty(operation, fences, false);
            }
            return;
        }
        self.start_navigation_cleanup(
            operation,
            handle,
            replacement,
            fences,
            match history {
                DirectoryHistory::Blank => CleanupContinuation::Done,
                DirectoryHistory::Fork => CleanupContinuation::ArchiveFork,
            },
            false,
        );
    }
}

impl App {
    fn prepare_directory_ready(
        &mut self,
        context: DirectoryContext,
    ) -> Result<DirectoryReady, String> {
        let DirectoryContext { input, mut config } = context;
        let selected_profile = input.permissions.selection();
        let cwd = &input.cwd;
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
            return Err("Permission profile has different settings.".into());
        }
        if let Some(profile) = profile.as_ref()
            && profile.turn_override == RuntimePermissionProfileTurnOverride::Preserve
            && profile.active_permission_profile.is_none()
            && !crate::app_server_session::permission_profile_is_safely_represented_by_sandbox_mode(
                &profile.permission_profile,
                cwd.as_path(),
            )
        {
            return Err("Permission profile cannot be preserved by /cd.".into());
        }
        if selected_profile.is_none()
            && let Err(error) = self.apply_runtime_policy_overrides(&mut config, scope)
        {
            return Err(format!("{error:#}"));
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
            return Err("Requested permission policy cannot be preserved by /cd.".into());
        }
        let local_settings = self.local_settings.reloaded(&config);
        let keymap = RuntimeKeymap::from_config(&local_settings.tui.keymap)?;
        config.service_tier = self.chat_widget.configured_service_tier();

        Ok(DirectoryReady {
            context: DirectoryContext { input, config },
            settings: local_settings,
            keymap,
        })
    }

    fn continue_directory_effect(
        &self,
        mut operation: PendingNavigation,
        handle: AppServerRequestHandle,
        stage: DirectoryStage,
        effect: DirectoryEffect,
    ) -> PendingNavigation {
        operation.intent = NavigationIntent::Directory(stage);
        self.spawn_directory_effect(&mut operation, handle, effect);
        operation
    }

    async fn advance_directory_trust(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        mut context: DirectoryContext,
        mut plan: DirectoryTrustPreparation,
    ) {
        if let Some(cwd) = plan.next_cwd() {
            let host = plan.host();
            self.server_operations.navigation.current = Some(self.continue_directory_effect(
                operation,
                server.request_handle(),
                DirectoryStage::Trust {
                    context: Box::new(context),
                    plan,
                },
                DirectoryEffect::Project { cwd, host },
            ));
            return;
        }
        // CurrentTask consent can select restricted settings without a write too.
        let _consent = plan.finish();
        match self
            .rebuild_config_for_cwd(context.input.cwd.to_path_buf())
            .await
        {
            Ok(config) => context.config = config,
            Err(error) => {
                self.working_directory_error(format!("Cannot reload folder settings: {error}"));
                return;
            }
        }
        self.directory_defaults_or_status(server, operation, context);
    }

    pub(super) async fn continue_directory_trust_saved(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        consent: &DirectoryTrustAdmission,
    ) {
        let mut plan = consent.plan.clone();
        assert!(
            plan.note_consent(OnboardingResult {
                directory_trust_persisted: true,
                selected_gateway: None,
                should_exit: false
            })
            .is_none()
        );
        Box::pin(self.advance_directory_trust(server, operation, consent.context.clone(), plan))
            .await;
    }

    fn directory_defaults_or_status(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        context: DirectoryContext,
    ) {
        let defaults = matches!(context.input.history, DirectoryHistory::Blank)
            || (self.runtime_permission_profile_override.is_none()
                && self
                    .config
                    .permissions
                    .active_permission_profile()
                    .is_some_and(|profile| profile.id == BUILT_IN_PERMISSION_PROFILE_WORKSPACE));
        if defaults {
            let cwd = context.input.cwd.to_path_buf();
            self.server_operations.navigation.current = Some(self.continue_directory_effect(
                operation,
                server.request_handle(),
                DirectoryStage::Defaults(Box::new(context)),
                DirectoryEffect::Defaults(cwd),
            ));
        } else {
            self.directory_load_status(server, operation, context);
        }
    }

    fn directory_load_status(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        context: DirectoryContext,
    ) {
        match self.prepare_directory_ready(context) {
            Ok(ready) => {
                let root = ready.context.input.thread_id();
                self.server_operations.navigation.current = Some(self.continue_directory_effect(
                    operation,
                    server.request_handle(),
                    DirectoryStage::Loaded(Box::new(ready)),
                    DirectoryEffect::Loaded(root),
                ));
            }
            Err(error) => self.working_directory_error(error),
        }
    }

    async fn acquire_directory_replacement(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        ready: Box<DirectoryReady>,
    ) {
        if !self
            .directory_authority_matches(&ready.context.input, false)
            .await
        {
            self.directory_stale();
            return;
        }
        let effect = match ready.context.input.history {
            DirectoryHistory::Blank => Acquisition::Start(
                server
                    .prepare_start_thread(
                        &ready.settings,
                        &ready.context.config,
                        ThreadStartChoice::Configured(None),
                    )
                    .with_selected_profile(ready.context.input.permissions.selection()),
            ),
            DirectoryHistory::Fork => Acquisition::Fork(server.prepare_fork_thread(
                RequestPolicy::Interactive,
                &ready.settings,
                ready.context.config.clone(),
                ready.context.input.thread_id(),
                None,
                None,
                crate::app_server_session::ForkGoalContinuation::DeferUntilNextTurn,
                ready.context.input.permissions.selection(),
                None,
            )),
        };
        self.server_operations.navigation.current = Some(self.continue_directory_effect(
            operation,
            server.request_handle(),
            DirectoryStage::Acquiring(ready),
            DirectoryEffect::Acquire(Box::new(effect)),
        ));
    }

    async fn detach_directory_source(
        &mut self,
        operation: PendingNavigation,
        handle: AppServerRequestHandle,
        replacement: Box<DirectoryReplacement>,
    ) {
        let input = &replacement.ready.context.input;
        let source = input.thread_id();
        let target = replacement.started.session.thread_id;
        if !self.directory_authority_matches(input, false).await {
            self.directory_stale();
            self.directory_cleanup(operation, handle, source, target, input.history, Vec::new());
            return;
        }
        let mut plan = self
            .prepare_detach_plan(DetachScope::Navigation {
                destination: Some(target),
            })
            .await;
        // Replace only the captured open graph, not closed or replay-only cached rows.
        plan.targets
            .retain(|entry| input.tracked.contains(&entry.thread_id));
        // /cd replaces the whole captured local task graph, including parked blanks.
        for &thread_id in &input.tracked {
            if thread_id != target
                && !plan
                    .targets
                    .iter()
                    .any(|entry| entry.thread_id == thread_id)
            {
                let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
                plan.targets.push(detach::DetachTarget {
                    thread_id,
                    store,
                    kind: detach::DetachKind::Conversation,
                });
            }
        }
        if !self.directory_authority_matches(input, false).await {
            self.directory_stale();
            self.directory_cleanup(operation, handle, source, target, input.history, Vec::new());
            return;
        }
        // The old primary remains usable if its unsubscribe is rejected. Do not detach
        // the rest of its graph until that prerequisite is confirmed.
        plan.targets.sort_by_key(|entry| entry.thread_id != source);
        assert_eq!(
            plan.targets.first().map(|entry| entry.thread_id),
            Some(source)
        );
        let effect = DirectoryEffect::Detach(plan.clone());
        self.server_operations.navigation.current = Some(self.continue_directory_effect(
            operation,
            handle,
            DirectoryStage::Detaching { replacement, plan },
            effect,
        ));
    }

    pub(super) async fn finish_directory_navigation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        matching: bool,
        receipt: DirectoryReceipt,
    ) -> Result<()> {
        let same_epoch = operation.epoch == self.server_operations.epoch;
        let NavigationIntent::Directory(stage) =
            std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("directory receipt without directory owner");
        };
        let DirectoryReceipt { handle, outcome } = receipt;
        match (stage, outcome) {
            (DirectoryStage::Config(input), DirectoryOutcome::Begin) if matching => {
                if !self.directory_authority_matches(&input, false).await {
                    self.directory_stale();
                    return Ok(());
                }
                let config = match self.rebuild_config_for_cwd(input.cwd.to_path_buf()).await {
                    Ok(config) => config,
                    Err(error) => {
                        self.working_directory_error(format!(
                            "Cannot load {}: {error}",
                            input.cwd.display()
                        ));
                        return Ok(());
                    }
                };
                let context = DirectoryContext {
                    input: *input,
                    config,
                };
                if context.config.active_project.trust_level.is_none() {
                    let ids = context.input.tracked.clone();
                    self.server_operations.navigation.current =
                        Some(self.continue_directory_effect(
                            operation,
                            handle,
                            DirectoryStage::BeforeTrust(Box::new(context)),
                            DirectoryEffect::Terminals {
                                ids,
                                target: self.app_server_target.clone(),
                            },
                        ));
                } else {
                    self.directory_defaults_or_status(server, operation, context);
                }
            }
            (DirectoryStage::BeforeTrust(context), DirectoryOutcome::Terminals(result))
                if matching =>
            {
                if let Err(error) = result {
                    self.working_directory_error(error);
                    return Ok(());
                }
                let plan = match DirectoryTrustPreparation::new(
                    &context.config,
                    &self.app_server_target,
                    context.input.cwd.as_path(),
                    crate::onboarding::DirectoryTrustOptions {
                        cancel: Some(crate::onboarding::TrustCancelAction::CurrentTask),
                        ..Default::default()
                    },
                ) {
                    Ok(plan) => plan,
                    Err(error) => {
                        self.working_directory_error(format!(
                            "Unable to check folder trust: {error}"
                        ));
                        return Ok(());
                    }
                };
                Box::pin(self.advance_directory_trust(server, operation, *context, plan)).await;
            }
            (DirectoryStage::Trust { context, mut plan }, DirectoryOutcome::Project(result))
                if matching =>
            {
                let project = match result.and_then(|project| plan.accept_project(project)) {
                    Ok(project) => project,
                    Err(error) => {
                        self.working_directory_error(format!(
                            "Unable to check folder trust: {error}"
                        ));
                        return Ok(());
                    }
                };
                if let Some(project) = project {
                    match plan.choose(tui, handle.clone(), project).await {
                        Ok(DirectoryTrustDecision::Consent(consent)) => {
                            self.transfer_directory_trust(
                                server,
                                operation,
                                DirectoryTrustAdmission {
                                    consent,
                                    context: *context,
                                    plan,
                                },
                            );
                            return Ok(());
                        }
                        Ok(DirectoryTrustDecision::Confirmed(consent)) => {
                            if plan.note_consent(consent).is_some() {
                                return Ok(());
                            }
                        }
                        Err(error) => {
                            self.working_directory_error(format!(
                                "Unable to check folder trust: {error}"
                            ));
                            return Ok(());
                        }
                    }
                }
                Box::pin(self.advance_directory_trust(server, operation, *context, plan)).await;
            }
            (DirectoryStage::Defaults(mut context), DirectoryOutcome::Defaults(result))
                if matching =>
            {
                match result {
                    Ok(Some(defaults)) => {
                        crate::projectless::apply_defaults(
                            &mut context.config,
                            &self.harness_overrides,
                            server,
                            &self.environment_manager,
                            &defaults,
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        self.working_directory_error(format!("Cannot read defaults: {error}"));
                        return Ok(());
                    }
                }
                self.directory_load_status(server, operation, *context);
            }
            (DirectoryStage::Loaded(mut ready), DirectoryOutcome::Loaded(result)) if matching => {
                let observations = match result {
                    Ok(observations) => observations,
                    Err(error) => {
                        self.working_directory_error(format!("Cannot check agent status: {error}"));
                        return Ok(());
                    }
                };
                let complete = self.apply_loaded_subagent_observations(
                    ready.context.input.thread_id(),
                    observations,
                );
                if !complete {
                    self.working_directory_error("Cannot check agent status.");
                    return Ok(());
                }
                for (id, agent) in self.agent_navigation.ordered_threads() {
                    if !agent.is_closed && !ready.context.input.tracked.contains(&id) {
                        ready.context.input.tracked.push(id);
                    }
                }
                let ids = ready.context.input.tracked.clone();
                self.server_operations.navigation.current = Some(self.continue_directory_effect(
                    operation,
                    handle,
                    DirectoryStage::Status(ready),
                    DirectoryEffect::Status {
                        ids,
                        target: self.app_server_target.clone(),
                    },
                ));
            }
            (DirectoryStage::Status(ready), DirectoryOutcome::Status(result)) if matching => {
                match result {
                    Ok(()) => {
                        self.acquire_directory_replacement(server, operation, ready)
                            .await;
                    }
                    Err(error) => self.working_directory_error(error),
                }
            }
            (DirectoryStage::Acquiring(ready), DirectoryOutcome::Acquired(prepared)) => {
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
                let source = ready.context.input.thread_id();
                let history = ready.context.input.history;
                match result {
                    Ok(started) if matching => {
                        let session = &started.session;
                        let config = &ready.context.config;
                        let permissions_match =
                            if let Some(selected) = ready.context.input.permissions.selection() {
                                session
                                    .active_permission_profile
                                    .as_ref()
                                    .is_some_and(|active| active.id == selected.profile_id)
                                    && selected
                                        .approval_policy
                                        .is_none_or(|policy| session.approval_policy == policy)
                                    && selected.approvals_reviewer.is_none_or(|reviewer| {
                                        session.approvals_reviewer == reviewer
                                    })
                            } else {
                                session.approval_policy.to_core()
                                    == config.permissions.approval_policy.value()
                                    && session.approvals_reviewer == config.approvals_reviewer
                                    && session.active_permission_profile
                                        == config.permissions.active_permission_profile()
                            };
                        if session.thread_id == source
                            || crate::session_resume::cwds_differ(
                                session.cwd.as_path(),
                                ready.context.input.cwd.as_path(),
                            )
                            || session.runtime_workspace_roots != config.workspace_roots
                            || !permissions_match
                        {
                            self.working_directory_error(
                                "Requested directory or permissions not applied.",
                            );
                            self.directory_cleanup(
                                operation,
                                handle,
                                source,
                                session.thread_id,
                                history,
                                Vec::new(),
                            );
                        } else {
                            self.detach_directory_source(
                                operation,
                                handle,
                                Box::new(DirectoryReplacement {
                                    ready: *ready,
                                    started,
                                }),
                            )
                            .await;
                        }
                    }
                    result => {
                        if matching && let Err(error) = result {
                            self.working_directory_error(format!("Failed to change: {error}"));
                        }
                        match custody {
                            SubscriptionCustody::Subscribed(target) => self.directory_cleanup(
                                operation,
                                handle,
                                source,
                                target,
                                history,
                                Vec::new(),
                            ),
                            SubscriptionCustody::Uncertain(target) if same_epoch => {
                                self.working_directory_error("Directory replacement outcome is unknown. No request was retried; close and reopen this TUI to resynchronize. The earlier request may still complete.");
                                self.retain_navigation_uncertainty(operation, vec![target], false);
                            }
                            SubscriptionCustody::NewThreadUncertain if same_epoch => {
                                self.working_directory_error("Directory replacement outcome is unknown. No request was retried; close and reopen this TUI to resynchronize. The earlier request may still complete.");
                                self.retain_navigation_uncertainty(operation, Vec::new(), false);
                            }
                            _ => {}
                        }
                    }
                }
            }
            (
                DirectoryStage::Detaching { replacement, plan },
                DirectoryOutcome::Detached(receipt),
            ) => {
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
                let input = &replacement.ready.context.input;
                let source = input.thread_id();
                let target = replacement.started.session.thread_id;
                let valid = matching
                    && fences.is_empty()
                    && self.directory_authority_matches(input, true).await;
                if valid {
                    let history = input.history;
                    let DirectoryReplacement { ready, started } = *replacement;
                    let DirectoryReady {
                        context,
                        settings,
                        keymap,
                    } = ready;
                    let cwd = context.input.cwd.clone();
                    Box::pin(self.attach_working_directory(
                        tui,
                        server,
                        DirectoryAttachment {
                            started,
                            config: Box::new(context.config),
                            local_settings: settings,
                            keymap,
                            cwd: cwd.clone(),
                            selected_profile: context.input.permissions.selection().cloned(),
                        },
                    ))
                    .await;
                    if self.current_displayed_thread_id() != Some(target) {
                        self.directory_detached_source();
                        self.directory_cleanup(
                            operation,
                            handle,
                            source,
                            target,
                            history,
                            Vec::new(),
                        );
                        return Ok(());
                    }
                    if !self.config.bypass_hook_trust {
                        self.server_operations.navigation.current =
                            Some(self.continue_directory_effect(
                                operation,
                                handle,
                                DirectoryStage::Hooks(target),
                                DirectoryEffect::Hooks(cwd.to_path_buf()),
                            ));
                    }
                } else {
                    if matching {
                        if fences.is_empty() {
                            self.directory_detached_source();
                        } else {
                            self.working_directory_error("Directory detachment was not confirmed. Close and reopen this TUI to resynchronize; no detach was retried.");
                        }
                    }
                    self.directory_cleanup(
                        operation,
                        handle,
                        source,
                        target,
                        input.history,
                        if same_epoch { fences } else { Vec::new() },
                    );
                }
            }
            (DirectoryStage::Hooks(thread_id), DirectoryOutcome::Hooks(result)) if matching => {
                if self.current_displayed_thread_id() == Some(thread_id) {
                    match result {
                        Ok(hooks)
                            if hooks.hooks.iter().any(crate::hooks_rpc::hook_needs_review) =>
                        {
                            self.chat_widget.open_hooks_browser(hooks);
                        }
                        Ok(_) => {}
                        Err(error) => self
                            .chat_widget
                            .add_error_message(format!("Cannot load folder hooks: {error}")),
                    }
                }
            }
            (stage, DirectoryOutcome::Terminated) => {
                if matching {
                    self.working_directory_error("Directory preparation terminated unexpectedly. No request was retried; close and reopen this TUI to resynchronize.");
                }
                match stage {
                    DirectoryStage::Acquiring(_) if same_epoch => {
                        self.retain_navigation_uncertainty(operation, Vec::new(), false);
                    }
                    DirectoryStage::Detaching { replacement, plan } => self.directory_cleanup(
                        operation,
                        handle,
                        replacement.ready.context.input.thread_id(),
                        replacement.started.session.thread_id,
                        replacement.ready.context.input.history,
                        if same_epoch {
                            plan.targets.iter().map(|target| target.thread_id).collect()
                        } else {
                            Vec::new()
                        },
                    ),
                    _ => {}
                }
            }
            (stage, _) if !matching && stage.is_read_only() => {}
            _ => unreachable!("directory stage received a different stage's receipt"),
        }
        Ok(())
    }
}
