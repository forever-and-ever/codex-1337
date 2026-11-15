//! Ordinary forks and explicitly authorized safety retries share acquisition custody.

use super::ACK_TIMEOUT;
use super::AbortOnDropHandle;
use super::App;
use super::AppCommand;
use super::AppRunControl;
use super::AppServerSession;
use super::AppServerStartedThread;
use super::Arc;
use super::ChatWidget;
use super::CleanupContinuation;
use super::ClientRequest;
use super::Config;
use super::DetachScope;
use super::DraftDisposition;
use super::Duration;
use super::FutureExt;
use super::LaneState;
use super::LocalDraftRecovery;
use super::NavigationCompletion;
use super::NavigationIntent;
use super::NavigationReceipt;
use super::OperationOrigin;
use super::PendingNavigation;
use super::PermissionAuthority;
use super::Result;
use super::SelectionAfter;
use super::ServerCompletion;
use super::SideThreadState;
use super::ThreadId;
use super::UserMessage;
use super::UserTurnReplay;
use super::detach;
use super::navigation_config;
use super::session_lifecycle;
use super::tui;
use super::ui_requests;
use crate::app::safety_buffering::safety_retry_fork_point;
use crate::app::thread_name::ThreadNameRequest;
use crate::app_server_session::ForkGoalContinuation;
use crate::app_server_session::ForkName;
use crate::app_server_session::fork_preparation::ForkPreparation;
use crate::app_server_session::session_preparation::PreparedSafetyHistory;
use crate::app_server_session::session_preparation::PreparedSession;
use crate::app_server_session::session_preparation::RequestPolicy;
use crate::app_server_session::session_preparation::SafetyHistoryPreparation;
use crate::app_server_session::session_preparation::SubscriptionCustody;
use crate::app_server_session::session_preparation::{self as preparation};
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol as api;
use codex_app_server_protocol::ThreadItemPayload;

pub(super) struct ForkInput {
    origin: OperationOrigin,
    permissions: PermissionAuthority,
    purpose: ForkPurpose,
}

impl ForkInput {
    #[expect(
        clippy::expect_used,
        reason = "Fork navigation is admitted only from a thread origin."
    )]
    fn thread_id(&self) -> ThreadId {
        self.origin.thread_id().expect("fork has a source thread")
    }
    fn take_draft(&mut self) -> Option<LocalDraftRecovery> {
        let prompt = match &mut self.purpose {
            ForkPurpose::Safety(retry) => &mut retry.prompt,
            ForkPurpose::Recourse { prompt, .. } | ForkPurpose::Side { prompt, .. } => prompt,
            ForkPurpose::Ordinary { .. } => return None,
        };
        prompt.take().map(|message| LocalDraftRecovery {
            origin: self.origin.clone(),
            message,
        })
    }
}

#[derive(Clone, Copy)]
enum SideForkScope {
    AllSides,
    KeepRetainedSides(super::Uuid),
}

enum ForkPurpose {
    Ordinary {
        name: Option<ForkName>,
        from_locked_thread: bool,
        from_agents_overview: bool,
        source_turn_running: bool,
    },
    Safety(Box<SafetyContinuation>),
    Recourse {
        turn_id: String,
        previous: Option<String>,
        prompt: Option<UserMessage>,
    },
    Side {
        prompt: Option<UserMessage>,
        scope: SideForkScope,
    },
}

struct SafetyContinuation {
    turn_id: String,
    turn: UserTurnReplay,
    prompt: Option<UserMessage>,
    latest_turn_id: Option<String>,
}

pub(super) struct ForkContext {
    input: ForkInput,
    config: Config,
}
pub(super) struct ForkReady {
    context: ForkContext,
    started: AppServerStartedThread,
    side_storage: Option<crate::app_server_session::SideChatStorage>,
}
pub(super) enum ForkStage {
    Admitted(Box<ForkInput>),
    Config(Box<ForkInput>),
    Interrupting(Box<ForkContext>),
    History(Box<ForkContext>),
    Acquiring(Box<ForkContext>),
    Detaching {
        ready: Box<ForkReady>,
        plan: detach::DetachPlan,
    },
}
impl ForkStage {
    pub(super) fn thread_id(&self) -> ThreadId {
        match self {
            Self::Admitted(input) | Self::Config(input) => input.thread_id(),
            Self::Interrupting(context) | Self::History(context) | Self::Acquiring(context) => {
                context.input.thread_id()
            }
            Self::Detaching { ready, .. } => ready.context.input.thread_id(),
        }
    }
    pub(super) fn fences_thread(&self, id: ThreadId) -> bool {
        self.thread_id() == id
            || matches!(self, Self::Detaching { ready, plan } if ready.started.session.thread_id == id || plan.targets.iter().any(|target| target.thread_id == id))
    }
    pub(super) fn is_read_only(&self) -> bool {
        matches!(self, Self::Admitted(_) | Self::Config(_) | Self::History(_))
    }
    pub(super) fn take_draft(&mut self) -> Option<LocalDraftRecovery> {
        match self {
            Self::Admitted(input) | Self::Config(input) => input.take_draft(),
            Self::Interrupting(context) | Self::History(context) | Self::Acquiring(context) => {
                context.input.take_draft()
            }
            Self::Detaching { ready, .. } => ready.context.input.take_draft(),
        }
    }
}

enum ForkEffect {
    Refresh(Box<navigation_config::ConfigRebuild>),
    Begin,
    Interrupt {
        thread_id: ThreadId,
        turn_id: String,
    },
    History(SafetyHistoryPreparation),
    Acquire(Box<ForkPreparation>),
    RecourseHistory {
        source: ThreadId,
        turn_id: String,
    },
    RecourseAcquire {
        plan: Box<ForkPreparation>,
        source: ThreadId,
        turn_id: String,
        previous: Option<String>,
    },
    Detach(detach::DetachPlan),
}
enum ForkOutcome {
    Refreshed(Result<Box<(Config, crate::local_settings::LocalSettings)>>),
    Begin,
    Interrupted(std::result::Result<(), TypedRequestError>),
    History(Box<PreparedSafetyHistory>),
    Acquired(Box<PreparedSession>),
    RecourseHistory(Result<Option<String>>),
    Detached(detach::DetachReceipt),
    Terminated,
}
pub(super) struct ForkReceipt {
    handle: AppServerRequestHandle,
    outcome: ForkOutcome,
}
impl ForkReceipt {
    pub(super) fn requires_owner(&self) -> bool {
        matches!(
            self.outcome,
            ForkOutcome::Interrupted(_) | ForkOutcome::Acquired(_) | ForkOutcome::Detached(_)
        )
    }
}

impl App {
    pub(in crate::app) fn start_side_fork(
        &mut self,
        app_server: &AppServerSession,
        parent_thread_id: ThreadId,
        user_message: Option<UserMessage>,
        workspace: bool,
    ) -> AppRunControl {
        self.session_telemetry
            .counter("codex.thread.side", 1, &[("source", "slash_command")]);
        self.enqueue_side_fork(app_server, parent_thread_id, user_message, workspace);
        AppRunControl::Continue
    }

    pub(in crate::app) fn enqueue_side_fork(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        prompt: Option<UserMessage>,
        workspace: bool,
    ) {
        let scope = if workspace {
            SideForkScope::KeepRetainedSides(super::Uuid::new_v4())
        } else {
            SideForkScope::AllSides
        };
        if let SideForkScope::KeepRetainedSides(request) = scope {
            let pane = self.sidebars.entry(Some(thread_id)).or_default();
            pane.creation = Some(crate::app::sidebar::SideCreation {
                owner: server.backend_id(),
                state: crate::app::sidebar::CreationState::Waiting {
                    request,
                    started: std::time::Instant::now(),
                },
                message: None,
            });
            pane.show(
                crate::app::sidebar::SidebarContent::Chooser,
                crate::app::sidebar::PaneFocus::Sidebar,
            );
        }
        let input = ForkInput {
            origin: OperationOrigin::Thread {
                thread_id,
                store: Arc::clone(&self.ensure_thread_channel(thread_id).store),
            },
            permissions: PermissionAuthority::capture(
                self.chat_widget.config_ref(),
                self.selected_server_profile(thread_id),
            ),
            purpose: ForkPurpose::Side { prompt, scope },
        };
        self.enqueue_navigation(
            server,
            NavigationIntent::Fork(ForkStage::Admitted(Box::new(input))),
        );
        self.chat_widget.fork_in_progress = true;
    }

    pub(in crate::app) fn enqueue_ordinary_fork(
        &mut self,
        server: &AppServerSession,
        name: Option<String>,
        conflict_thread_ids: Vec<String>,
        from_agents_overview: bool,
    ) {
        let Some(thread_id) = self.chat_widget.thread_id() else {
            return;
        };
        self.supersede_pending_navigation();
        let generation = self.server_operations.navigation.generation();
        let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
        let purpose = ForkPurpose::Ordinary {
            name: name.map(|name| ForkName {
                name,
                conflict_thread_ids,
            }),
            from_locked_thread: self.chat_widget.is_external_writer_view(),
            from_agents_overview,
            source_turn_running: self.chat_widget.is_agent_turn_running(),
        };
        self.request_server_prepared(
            server,
            ui_requests::UiRequestScope::Thread(thread_id),
            ui_requests::UiRequestKind::ReadOnly,
            "Preparing fork settings",
            move |app, _| {
                let current = app.current_displayed_thread_id() == Some(thread_id)
                    && app.chat_widget.thread_id() == Some(thread_id)
                    && app.server_operations.navigation.generation() == generation
                    && app
                        .thread_event_channels
                        .get(&thread_id)
                        .is_some_and(|channel| Arc::ptr_eq(&channel.store, &store));
                let input = current.then(|| ForkInput {
                    origin: OperationOrigin::Thread { thread_id, store },
                    permissions: PermissionAuthority::capture(
                        app.chat_widget.config_ref(),
                        app.selected_server_profile(thread_id),
                    ),
                    purpose,
                });
                move |_| async move { Ok(input) }
            },
            move |app, server, result| {
                if app.server_operations.navigation.generation() != generation {
                    return Ok(());
                }
                match result {
                    Ok(Some(input)) if app.current_displayed_thread_id() == Some(thread_id) => {
                        app.enqueue_navigation(
                            server,
                            NavigationIntent::Fork(ForkStage::Admitted(Box::new(input))),
                        );
                        app.chat_widget.fork_in_progress = true;
                    }
                    Err(error) => app
                        .chat_widget
                        .add_error_message(format!("Failed to prepare fork settings: {error}")),
                    _ => {}
                }
                Ok(())
            },
        );
    }

    pub(in crate::app) fn enqueue_safety_recourse(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        turn_id: String,
        prompt: String,
    ) {
        if !crate::app::safety_recourse::supports_server(server)
            || self.active_thread_id != Some(thread_id)
            || self.primary_thread_id != Some(thread_id)
            || self.chat_widget.last_turn_id() != Some(turn_id.as_str())
            || self.chat_widget.is_user_turn_pending_or_running()
            || self.chat_widget.has_queued_follow_up_messages()
            || self.pending_server_profiles.contains_key(&thread_id)
        {
            return;
        }
        let input = ForkInput {
            origin: OperationOrigin::Thread {
                thread_id,
                store: Arc::clone(&self.ensure_thread_channel(thread_id).store),
            },
            permissions: PermissionAuthority::capture(
                self.chat_widget.config_ref(),
                self.selected_server_profile(thread_id),
            ),
            purpose: ForkPurpose::Recourse {
                turn_id,
                previous: None,
                prompt: Some(prompt.into()),
            },
        };
        self.enqueue_navigation(
            server,
            NavigationIntent::Fork(ForkStage::Admitted(Box::new(input))),
        );
    }

    #[expect(
        clippy::expect_used,
        reason = "The safety producer admits only validated user turns."
    )]
    pub(in crate::app) async fn enqueue_safety_fork(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        turn_id: String,
        turn: AppCommand,
        prompt: UserMessage,
    ) {
        let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
        let latest_turn_id = store.lock().await.latest_turn_id.clone();
        let input = ForkInput {
            origin: OperationOrigin::Thread { thread_id, store },
            permissions: PermissionAuthority::capture(
                self.chat_widget.config_ref(),
                self.selected_server_profile(thread_id),
            ),
            purpose: ForkPurpose::Safety(Box::new(SafetyContinuation {
                turn_id,
                turn: UserTurnReplay::new(turn).expect("safety producer validated a user turn"),
                prompt: Some(prompt),
                latest_turn_id,
            })),
        };
        self.enqueue_navigation(
            server,
            NavigationIntent::Fork(ForkStage::Admitted(Box::new(input))),
        );
    }

    pub(super) fn cancel_fork_projection(&mut self, source: ThreadId) {
        if self.chat_widget.thread_id() == Some(source) {
            self.chat_widget.fork_in_progress = false;
        }
    }

    pub(super) fn dispatch_fork_navigation(
        &self,
        server: &AppServerSession,
        operation: &mut PendingNavigation,
    ) {
        let NavigationIntent::Fork(ForkStage::Admitted(input)) =
            std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("fork dispatch requires admission");
        };
        let effect = if matches!(
            input.purpose,
            ForkPurpose::Safety(_) | ForkPurpose::Recourse { .. }
        ) {
            ForkEffect::Begin
        } else {
            ForkEffect::Refresh(
                self.capture_config_rebuild(self.chat_widget.config_ref().cwd.to_path_buf(), None),
            )
        };
        operation.intent = NavigationIntent::Fork(ForkStage::Config(input));
        self.spawn_fork_effect(operation, server.request_handle(), effect);
    }

    fn spawn_fork_effect(
        &self,
        operation: &mut PendingNavigation,
        handle: AppServerRequestHandle,
        effect: ForkEffect,
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
                        ForkEffect::Begin => ForkOutcome::Begin,
                        ForkEffect::Refresh(plan) => ForkOutcome::Refreshed(plan.run().await),
                        ForkEffect::Interrupt { thread_id, turn_id } => ForkOutcome::Interrupted(
                            preparation::request::<api::TurnInterruptResponse>(
                                RequestPolicy::Interactive,
                                &handle,
                                ClientRequest::TurnInterrupt {
                                    request_id: api::RequestId::String(format!(
                                        "tui-safety-interrupt-{id}"
                                    )),
                                    params: api::TurnInterruptParams {
                                        thread_id: thread_id.to_string(),
                                        turn_id,
                                    },
                                },
                            )
                            .await
                            .map(|_| ()),
                        ),
                        ForkEffect::History(plan) => {
                            ForkOutcome::History(Box::new(plan.run().await))
                        }
                        ForkEffect::RecourseHistory { source, turn_id } => {
                            ForkOutcome::RecourseHistory(
                                crate::app::safety_recourse::validate_source(
                                    &handle, source, &turn_id,
                                )
                                .await,
                            )
                        }
                        ForkEffect::RecourseAcquire {
                            plan,
                            source,
                            turn_id,
                            previous,
                        } => {
                            let mut prepared = Box::pin(plan.run()).await;
                            if let Ok(started) = &prepared.result
                                && let Err(error) = crate::app::safety_recourse::verify_fork(
                                    &handle,
                                    started,
                                    source,
                                    &turn_id,
                                    previous.as_deref(),
                                )
                                .await
                            {
                                prepared.result = Err(error);
                            }
                            ForkOutcome::Acquired(Box::new(prepared))
                        }
                        ForkEffect::Acquire(plan) => {
                            ForkOutcome::Acquired(Box::new(Box::pin(plan.run()).await))
                        }
                        ForkEffect::Detach(plan) => ForkOutcome::Detached(
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
                    }
                })
                .catch_unwind(),
            )
            .await
            .unwrap_or(ForkOutcome::Terminated);
            let _ = completions.send(ServerCompletion::Navigation(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::Fork(ForkReceipt { handle, outcome }),
                },
            )));
        })));
    }

    fn continue_fork_effect(
        &self,
        mut operation: PendingNavigation,
        handle: AppServerRequestHandle,
        stage: ForkStage,
        effect: ForkEffect,
    ) -> PendingNavigation {
        operation.intent = NavigationIntent::Fork(stage);
        self.spawn_fork_effect(&mut operation, handle, effect);
        operation
    }

    async fn fork_authority_matches(&self, input: &ForkInput) -> bool {
        let (source, store) = input.origin.thread();
        if self.current_displayed_thread_id() != Some(source)
            || self.chat_widget.thread_id() != Some(source)
            || self.pending_server_profiles.contains_key(&source)
            || !input.permissions.matches(
                self.chat_widget.config_ref(),
                self.agents_overview
                    .requested_permission_profiles
                    .get(&source),
            )
            || !self
                .thread_event_channels
                .get(&source)
                .is_some_and(|channel| Arc::ptr_eq(&channel.store, store))
        {
            return false;
        }
        if let ForkPurpose::Recourse {
            turn_id, prompt, ..
        } = &input.purpose
        {
            let store = store.lock().await;
            return self.primary_thread_id == Some(source)
                && store.latest_turn_id.as_deref() == Some(turn_id.as_str())
                && store.active_turn_id().is_none()
                && prompt.is_some()
                && !self.chat_widget.is_user_turn_pending_or_running()
                && !self.chat_widget.has_queued_follow_up_messages();
        }
        if let ForkPurpose::Safety(retry) = &input.purpose {
            let store = store.lock().await;
            self.primary_thread_id == Some(source)
                && self.active_thread_id == Some(source)
                && store.latest_turn_id == retry.latest_turn_id
                && store.active_turn_id().is_none_or(|id| id == retry.turn_id)
                && retry.prompt.is_some()
        } else {
            true
        }
    }

    fn recover_fork_input(&mut self, input: &mut ForkInput) {
        if let ForkPurpose::Side {
            scope: SideForkScope::KeepRetainedSides(request),
            ..
        } = &input.purpose
            && let Some(pane) = self.sidebars.get_mut(&Some(input.thread_id()))
            && pane
                .creation
                .as_ref()
                .is_some_and(|creation| creation.request() == Some(*request))
        {
            pane.creation = None;
        }
        if let Some(recovery) = input.take_draft() {
            self.queue_local_draft_recovery(recovery);
        }
    }

    fn fork_failure(
        &mut self,
        input: &mut ForkInput,
        error: impl std::fmt::Display,
        matching: bool,
    ) {
        if matching
            && let ForkPurpose::Side {
                scope: SideForkScope::KeepRetainedSides(_),
                prompt,
            } = &mut input.purpose
        {
            let prompt = prompt.take();
            let parent = input.origin.thread().0;
            self.fail_side_creation(
                parent,
                App::side_start_error_message(&color_eyre::eyre::eyre!("{error}")),
                prompt,
                None,
            );
            self.cancel_fork_projection(parent);
            return;
        }
        self.recover_fork_input(input);
        if matching {
            self.cancel_fork_projection(input.thread_id());
            let text = match input.purpose {
                ForkPurpose::Side { .. } => {
                    App::side_start_error_message(&color_eyre::eyre::eyre!("{error}"))
                }
                ForkPurpose::Safety(_) => format!("Failed to retry with a faster model: {error}"),
                ForkPurpose::Recourse { .. } => {
                    format!("Couldn’t start a new chat for this rewrite: {error}")
                }
                ForkPurpose::Ordinary { .. } => {
                    format!("Failed to fork current session through the app server: {error}")
                }
            };
            self.chat_widget.add_error_message(text);
        }
    }

    fn acquire_fork_navigation(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        context: Box<ForkContext>,
    ) {
        if let ForkPurpose::Side { scope, .. } = &context.input.purpose {
            let plan = server
                .prepare_side_thread(
                    RequestPolicy::Interactive,
                    &self.local_settings,
                    context.config.clone(),
                    context.input.thread_id(),
                    context.input.permissions.selection(),
                )
                .with_storage(if matches!(scope, SideForkScope::KeepRetainedSides(_)) {
                    crate::app_server_session::SideChatStorage::Attached
                } else {
                    crate::app_server_session::SideChatStorage::Temporary
                });
            self.server_operations.navigation.current = Some(self.continue_fork_effect(
                operation,
                server.request_handle(),
                ForkStage::Acquiring(context),
                ForkEffect::Acquire(Box::new(plan)),
            ));
            return;
        }
        let (before_turn_id, goal_continuation, name) = match &context.input.purpose {
            ForkPurpose::Ordinary {
                name,
                source_turn_running,
                ..
            } => (
                None,
                if *source_turn_running {
                    ForkGoalContinuation::DeferUntilNextTurn
                } else {
                    ForkGoalContinuation::StartIfIdle
                },
                name.clone(),
            ),
            ForkPurpose::Side { .. } => unreachable!("side fork prepared separately"),
            ForkPurpose::Recourse { turn_id, .. } => (
                Some(turn_id.clone()),
                ForkGoalContinuation::StartIfIdle,
                None,
            ),
            ForkPurpose::Safety(retry) => (
                Some(retry.turn_id.clone()),
                ForkGoalContinuation::DeferUntilNextTurn,
                None,
            ),
        };
        let plan = server.prepare_fork_thread(
            RequestPolicy::Interactive,
            &self.local_settings,
            context.config.clone(),
            context.input.thread_id(),
            None,
            before_turn_id,
            goal_continuation,
            context.input.permissions.selection(),
            name,
        );
        let effect = match &context.input.purpose {
            ForkPurpose::Recourse {
                turn_id, previous, ..
            } => ForkEffect::RecourseAcquire {
                plan: Box::new(plan),
                source: context.input.thread_id(),
                turn_id: turn_id.clone(),
                previous: previous.clone(),
            },
            _ => ForkEffect::Acquire(Box::new(plan)),
        };
        self.server_operations.navigation.current = Some(self.continue_fork_effect(
            operation,
            server.request_handle(),
            ForkStage::Acquiring(context),
            effect,
        ));
    }
}

impl App {
    async fn retain_fork_origin(&mut self, source: ThreadId) {
        let mut retained = self.agent_navigation.tracked_thread_ids();
        retained.push(source);
        retained.extend(
            self.thread_event_channels
                .keys()
                .copied()
                .filter(|id| self.is_family_thread(*id)),
        );
        for thread_id in retained {
            if !self.side_threads.contains_key(&thread_id) {
                self.agents_overview
                    .dispatched_requests
                    .entry(thread_id)
                    .or_default();
                if let Some(channel) = self.thread_event_channels.get(&thread_id)
                    && let Some(input) = channel.store.lock().await.input_state.clone()
                {
                    self.agents_overview.input_states.insert(thread_id, input);
                }
            }
        }
        if let Some(input) = self.chat_widget.capture_thread_input_state() {
            self.agents_overview.input_states.insert(source, input);
        }
    }

    async fn detach_for_fork(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        handle: AppServerRequestHandle,
        mut ready: Box<ForkReady>,
    ) {
        if !self.fork_authority_matches(&ready.context.input).await {
            self.fork_failure(
                &mut ready.context.input,
                "Fork canceled because its source task changed.",
                true,
            );
            self.start_navigation_cleanup(
                operation,
                handle,
                ready.started.session.thread_id,
                Vec::new(),
                CleanupContinuation::Done,
                false,
            );
            return;
        }
        if matches!(
            ready.context.input.purpose,
            ForkPurpose::Ordinary { .. } | ForkPurpose::Side { .. } | ForkPurpose::Recourse { .. }
        ) {
            self.retain_fork_origin(ready.context.input.thread_id())
                .await;
        }
        let target = ready.started.session.thread_id;
        let scope = if matches!(
            ready.context.input.purpose,
            ForkPurpose::Side {
                scope: SideForkScope::KeepRetainedSides(_),
                ..
            }
        ) {
            DetachScope::NavigationKeepingRetainedSides {
                destination: target,
            }
        } else {
            DetachScope::Navigation {
                destination: Some(target),
            }
        };
        let plan = self.prepare_detach_plan(scope).await;
        let effect = ForkEffect::Detach(plan.clone());
        self.server_operations.navigation.current = Some(self.continue_fork_effect(
            operation,
            server.request_handle(),
            ForkStage::Detaching { ready, plan },
            effect,
        ));
    }

    #[expect(
        clippy::expect_used,
        reason = "Successful fork installation creates the target session before preparing its retry."
    )]
    async fn commit_fork_navigation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        operation: PendingNavigation,
        handle: AppServerRequestHandle,
        mut ready: ForkReady,
    ) -> Result<()> {
        let target = ready.started.session.thread_id;
        let has_authoritative_mode = ready.started.session.collaboration_mode.is_some();
        if matches!(ready.context.input.purpose, ForkPurpose::Ordinary { .. }) {
            let (source, store) = ready.context.input.origin.thread();
            let drafts = self.take_unsent_thread_operations(source, store).await;
            self.restore_submission_drafts(source, store, drafts, DraftDisposition::Edit)
                .await;
        }
        if matches!(ready.context.input.purpose, ForkPurpose::Side { .. }) {
            let parent = ready.context.input.origin.thread().0;
            let channel = self.ensure_thread_channel(target);
            {
                let mut store = channel.store.lock().await;
                App::install_side_thread_snapshot(
                    &mut store,
                    ready.started.session,
                    ready.started.turns,
                );
            }
            let mut side = SideThreadState::new(parent);
            side.storage = ready
                .side_storage
                .unwrap_or(crate::app_server_session::SideChatStorage::Temporary);
            self.side_threads.insert(target, side);
            if let ForkPurpose::Side {
                scope: SideForkScope::KeepRetainedSides(_),
                ..
            } = &ready.context.input.purpose
            {
                let pane = self.sidebars.entry(Some(parent)).or_default();
                pane.chat_ready(target);
            }
            self.upsert_agent_picker_thread(target, None, None, false);
            if let Some(profile) = ready.context.input.permissions.selection() {
                self.agents_overview
                    .selected_permission_profiles
                    .insert(target, profile.profile_id.clone());
            }
            let result = self
                .select_tracked_side_locally(tui, server, target, SelectionAfter::SelectOnly)
                .await;
            if let Err(error) = result {
                self.fork_failure(&mut ready.context.input, error, true);
                self.start_navigation_cleanup(
                    operation,
                    handle,
                    target,
                    Vec::new(),
                    CleanupContinuation::Done,
                    false,
                );
            } else {
                if ready.context.input.permissions.selection().is_some() {
                    self.adopt_inherited_server_selection();
                }
                if let ForkPurpose::Side { prompt, .. } = &mut ready.context.input.purpose
                    && let Some(message) = prompt.take()
                    && let Err(message) =
                        self.chat_widget.submit_plain_user_turn_to(target, message)
                {
                    self.restore_side_user_message(Some(message));
                }
            }
            return Ok(());
        }
        let retained_input = match &ready.context.input.purpose {
            ForkPurpose::Ordinary {
                from_locked_thread: true,
                source_turn_running: false,
                ..
            }
            | ForkPurpose::Safety(_)
            | ForkPurpose::Recourse { .. } => self.chat_widget.capture_thread_input_state(),
            ForkPurpose::Ordinary { .. } | ForkPurpose::Side { .. } => None,
        };
        if let Some(profile) = ready.context.input.permissions.selection() {
            self.agents_overview
                .selected_permission_profiles
                .insert(target, profile.profile_id.clone());
        }
        if matches!(
            ready.context.input.purpose,
            ForkPurpose::Safety(_) | ForkPurpose::Recourse { .. }
        ) {
            self.config = ready.context.config.clone();
        }
        let attached = Box::pin(self.replace_chat_widget_with_app_server_thread(
            tui,
            ready.started,
            session_lifecycle::ThreadAttachPresentation::SessionLineage,
            None,
        ))
        .await;
        if let Err(error) = attached {
            self.fork_failure(
                &mut ready.context.input,
                format!("Failed to attach fork: {error}"),
                true,
            );
            if self.current_displayed_thread_id() != Some(target) {
                if matches!(ready.context.input.purpose, ForkPurpose::Safety(_)) {
                    self.chat_widget.add_warning_message("The original task was detached. Unsent input is retained; reopen the original task to continue.".into());
                }
                self.start_navigation_cleanup(
                    operation,
                    handle,
                    target,
                    Vec::new(),
                    CleanupContinuation::Done,
                    false,
                );
            }
            return Ok(());
        }
        if ready.context.input.permissions.selection().is_some() {
            self.adopt_inherited_server_selection();
        }
        let accepted_mode =
            has_authoritative_mode.then(|| self.chat_widget.effective_collaboration_mode());
        match ready.context.input.purpose {
            ForkPurpose::Ordinary {
                from_locked_thread,
                from_agents_overview,
                ..
            } => {
                self.chat_widget.restore_reconnected_input(
                    retained_input,
                    &[],
                    crate::chatwidget::ThreadInputSettingsAuthority::Session { accepted_mode },
                );
                self.chat_widget
                    .add_info_message("Fork created. You can continue here.".into(), None);
                if (from_locked_thread || from_agents_overview)
                    && tui.presentation_mode() != tui::PresentationMode::Frozen
                {
                    if let Err(error) = tui.discard_pending_input_before_interactive_screen() {
                        tracing::warn!(%error, "failed to discard input after forking");
                    }
                    tui.schedule_screen_size_recheck(Duration::ZERO);
                }
            }
            ForkPurpose::Side { .. } => unreachable!("side committed separately"),
            ForkPurpose::Recourse { mut prompt, .. } => {
                self.chat_widget.restore_bound_thread_input_state(
                    retained_input,
                    crate::chatwidget::ThreadInputStateRestoreMode {
                        preserve_in_flight_turn: false,
                    },
                    crate::chatwidget::ThreadInputSettingsAuthority::Session { accepted_mode },
                );
                let Some(message) = prompt.take() else {
                    unreachable!("recourse retains its selected prompt")
                };
                let Some(turn) = self
                    .chat_widget
                    .prepare_safety_recourse_rewrite(&message.text)
                else {
                    self.chat_widget.add_error_message("The rewrite could not be admitted. Its prompt was restored without submitting.".into());
                    self.chat_widget.restore_user_message_to_composer(message);
                    return Ok(());
                };
                if let Err(error) = self.submit_thread_op(server, target, turn).await {
                    self.chat_widget.handle_turn_start_rejection(format!(
                        "Couldn’t submit this rewrite: {error}"
                    ));
                    self.chat_widget.restore_user_message_to_composer(message);
                }
            }
            ForkPurpose::Safety(mut retry) => {
                let Some(prompt) = retry.prompt.take() else {
                    unreachable!("matching retry consumed its prompt before installation");
                };
                let retry_display =
                    ChatWidget::user_message_display_from_inputs(retry.turn.input());
                let session = self.thread_event_channels[&target]
                    .store
                    .lock()
                    .await
                    .session
                    .clone()
                    .expect("installed fork session");
                retry.turn.inherit_permissions(&session);
                self.chat_widget.restore_bound_thread_input_state(
                    retained_input,
                    crate::chatwidget::ThreadInputStateRestoreMode {
                        preserve_in_flight_turn: false,
                    },
                    crate::chatwidget::ThreadInputSettingsAuthority::Session { accepted_mode },
                );
                self.chat_widget
                    .prepare_safety_buffered_retry_submission(prompt.clone());
                if let Err(error) = self
                    .submit_thread_op(server, target, retry.turn.into_command())
                    .await
                {
                    self.chat_widget.cancel_safety_buffered_retry_submission();
                    self.chat_widget.restore_user_message_to_composer(prompt);
                    self.chat_widget
                        .add_error_message(format!("Failed to retry with a faster model: {error}"));
                } else {
                    self.chat_widget
                        .commit_safety_buffered_retry_submission(retry_display);
                }
            }
        }
        Ok(())
    }

    pub(super) async fn finish_fork_navigation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        matching: bool,
        receipt: ForkReceipt,
    ) -> Result<()> {
        let same_epoch = operation.epoch == self.server_operations.epoch;
        let NavigationIntent::Fork(stage) =
            std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("fork receipt without fork stage");
        };
        let ForkReceipt { handle, outcome } = receipt;
        match (stage, outcome) {
            (
                ForkStage::Config(mut input),
                outcome @ (ForkOutcome::Begin | ForkOutcome::Refreshed(_)),
            ) if matching => {
                if !self.fork_authority_matches(&input).await {
                    self.fork_failure(
                        &mut input,
                        "Fork canceled because its source task changed.",
                        true,
                    );
                    return Ok(());
                }
                if let ForkOutcome::Refreshed(result) = outcome {
                    self.apply_navigation_config_refresh(result, "forking the thread");
                }
                let config = match input.purpose {
                    ForkPurpose::Ordinary { .. } => {
                        let mut config = if self.chat_widget.is_parent_owned_thread() {
                            let mut config = self.chat_widget.config_ref().clone();
                            config.service_tier =
                                self.chat_widget.current_service_tier().map(str::to_string);
                            config
                        } else {
                            self.config.clone()
                        };
                        if server.uses_remote_workspace() {
                            config
                                .workspace_roots
                                .clone_from(&self.chat_widget.config_ref().workspace_roots);
                        }
                        config
                            .model_provider_id
                            .clone_from(&self.chat_widget.config_ref().model_provider_id);
                        config.model = Some(self.chat_widget.current_model().to_string());
                        config.model_reasoning_effort = self.chat_widget.current_reasoning_effort();
                        config.daybreak_enabled = self.chat_widget.daybreak_enabled;
                        config
                    }
                    ForkPurpose::Side { .. } => self.side_fork_config(),
                    ForkPurpose::Recourse { .. } => {
                        let mut config = self.chat_widget.config_ref().clone();
                        config.model = Some(self.chat_widget.current_model().to_string());
                        config.model_reasoning_effort = self.chat_widget.current_reasoning_effort();
                        config.daybreak_enabled = self.chat_widget.daybreak_enabled;
                        config
                    }
                    ForkPurpose::Safety(_) => self.chat_widget.config_ref().clone(),
                };
                let context = Box::new(ForkContext {
                    input: *input,
                    config,
                });
                if let ForkPurpose::Recourse { turn_id, .. } = &context.input.purpose {
                    let effect = ForkEffect::RecourseHistory {
                        source: context.input.thread_id(),
                        turn_id: turn_id.clone(),
                    };
                    self.server_operations.navigation.current = Some(self.continue_fork_effect(
                        operation,
                        handle,
                        ForkStage::History(context),
                        effect,
                    ));
                } else if let ForkPurpose::Safety(retry) = &context.input.purpose {
                    let effect = ForkEffect::Interrupt {
                        thread_id: context.input.thread_id(),
                        turn_id: retry.turn_id.clone(),
                    };
                    self.server_operations.navigation.current = Some(self.continue_fork_effect(
                        operation,
                        handle,
                        ForkStage::Interrupting(context),
                        effect,
                    ));
                } else {
                    self.acquire_fork_navigation(server, operation, context);
                }
            }
            (ForkStage::Interrupting(mut context), ForkOutcome::Interrupted(result)) => {
                match result {
                    Ok(()) if matching && self.fork_authority_matches(&context.input).await => {
                        let ForkPurpose::Safety(retry) = &context.input.purpose else {
                            unreachable!("ordinary fork cannot interrupt");
                        };
                        let plan = server.prepare_safety_retry_history(
                            RequestPolicy::Interactive,
                            context.input.thread_id(),
                            retry.turn_id.clone(),
                        );
                        self.server_operations.navigation.current =
                            Some(self.continue_fork_effect(
                                operation,
                                handle,
                                ForkStage::History(context),
                                ForkEffect::History(plan),
                            ));
                    }
                    result => {
                        let uncertain = result
                            .as_ref()
                            .is_err_and(|error| !matches!(error, TypedRequestError::Server { .. }));
                        let error = result.err().map_or_else(
                            || "Retry canceled after stopping the original attempt.".into(),
                            |error| error.to_string(),
                        );
                        self.fork_failure(&mut context.input, error, matching);
                        if uncertain && same_epoch {
                            let source = context.input.thread_id();
                            self.retain_navigation_uncertainty(operation, vec![source], false);
                        }
                    }
                }
            }
            (ForkStage::History(mut context), ForkOutcome::RecourseHistory(result)) => match result
            {
                Ok(previous) if matching && self.fork_authority_matches(&context.input).await => {
                    let ForkPurpose::Recourse {
                        previous: expected, ..
                    } = &mut context.input.purpose
                    else {
                        unreachable!("recourse owns validation")
                    };
                    *expected = previous;
                    self.acquire_fork_navigation(server, operation, context);
                }
                result => self.fork_failure(
                    &mut context.input,
                    result.err().map_or_else(
                        || "Rewrite canceled because its source changed.".into(),
                        |error| error.to_string(),
                    ),
                    matching,
                ),
            },
            (ForkStage::History(mut context), ForkOutcome::History(prepared)) if matching => {
                if !server.accepts_session_observations(&prepared.observations) {
                    self.fork_failure(
                        &mut context.input,
                        "prepared safety history belongs to a different backend",
                        matching,
                    );
                    return Ok(());
                }
                server.apply_session_observations(prepared.observations)?;
                let thread = match prepared.result {
                    Ok(thread) => thread,
                    Err(error) => {
                        self.fork_failure(&mut context.input, error, true);
                        return Ok(());
                    }
                };
                let ForkPurpose::Safety(retry) = &mut context.input.purpose else {
                    unreachable!("ordinary fork cannot read retry history");
                };
                if let Err(error) = safety_retry_fork_point(&thread.turns, &retry.turn_id) {
                    self.fork_failure(&mut context.input, error, true);
                    return Ok(());
                }
                retry.turn.extend_input(
                    thread
                        .turns
                        .iter()
                        .find(|turn| turn.id == retry.turn_id)
                        .into_iter()
                        .flat_map(api::Turn::item_payloads)
                        .filter_map(|item| match item {
                            ThreadItemPayload::UserMessage { content, .. } => Some(content),
                            _ => None,
                        })
                        .skip(1)
                        .flat_map(|content| {
                            std::iter::once(api::UserInput::Text {
                                text: "\n".into(),
                                text_elements: Vec::new(),
                            })
                            .chain(content.iter().cloned())
                        }),
                );
                if self.fork_authority_matches(&context.input).await {
                    self.acquire_fork_navigation(server, operation, context);
                } else {
                    self.fork_failure(
                        &mut context.input,
                        "Retry canceled because its source task changed.",
                        true,
                    );
                }
            }
            (ForkStage::Acquiring(mut context), ForkOutcome::Acquired(prepared)) => {
                let PreparedSession {
                    result,
                    custody,
                    observations,
                } = *prepared;
                let side_storage = observations.side_storage;
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
                if matching {
                    self.cancel_fork_projection(context.input.thread_id());
                }
                match result {
                    Ok(started)
                        if matching && self.fork_authority_matches(&context.input).await =>
                    {
                        if started.session.thread_id == context.input.thread_id() {
                            self.fork_failure(
                                &mut context.input,
                                "Fork response returned its source instead of a new thread.",
                                true,
                            );
                        } else {
                            self.detach_for_fork(
                                server,
                                operation,
                                handle,
                                Box::new(ForkReady {
                                    context: *context,
                                    started,
                                    side_storage,
                                }),
                            )
                            .await;
                        }
                    }
                    result => {
                        let mut name_conflict = false;
                        if matching
                            && let Err(error) = &result
                            && let ForkPurpose::Ordinary {
                                name: Some(name), ..
                            } = &context.input.purpose
                            && let Some(error) = error.downcast_ref::<TypedRequestError>()
                        {
                            name_conflict = self.confirm_thread_name_conflict(
                                error,
                                ThreadNameRequest::Fork {
                                    thread_id: context.input.thread_id(),
                                    name: name.name.clone(),
                                },
                            );
                        }
                        if !name_conflict {
                            self.fork_failure(
                                &mut context.input,
                                result.err().map_or_else(
                                    || "Fork canceled because its source task changed.".into(),
                                    |error| error.to_string(),
                                ),
                                matching,
                            );
                        }
                        if matching
                            && side_storage
                                == Some(crate::app_server_session::SideChatStorage::Attached)
                            && let SubscriptionCustody::Subscribed(target) = custody
                            && target != context.input.thread_id()
                            && let ForkPurpose::Side {
                                scope: SideForkScope::KeepRetainedSides(_),
                                ..
                            } = &context.input.purpose
                        {
                            let parent = context.input.thread_id();
                            let mut side = SideThreadState::new(parent);
                            side.storage = crate::app_server_session::SideChatStorage::Attached;
                            self.side_threads.insert(target, side);
                            self.ensure_thread_channel(target);
                            if let Some(pane) = self.sidebars.get_mut(&Some(parent))
                                && let Some(creation) = &mut pane.creation
                                && let crate::app::sidebar::CreationState::Failed {
                                    cleanup, ..
                                } = &mut creation.state
                            {
                                *cleanup = Some(target);
                            }
                            return Ok(());
                        }
                        match custody {
                            SubscriptionCustody::Subscribed(target)
                                if target != context.input.thread_id() =>
                            {
                                self.start_navigation_cleanup(
                                    operation,
                                    handle,
                                    target,
                                    Vec::new(),
                                    CleanupContinuation::Done,
                                    false,
                                );
                            }
                            SubscriptionCustody::Uncertain(target) if same_epoch => {
                                self.retain_navigation_uncertainty(operation, vec![target], false);
                            }
                            SubscriptionCustody::NewThreadUncertain if same_epoch => {
                                self.retain_navigation_uncertainty(operation, Vec::new(), false);
                            }
                            _ => {}
                        }
                    }
                }
            }
            (ForkStage::Detaching { mut ready, plan }, ForkOutcome::Detached(receipt)) => {
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
                if matching
                    && fences.is_empty()
                    && self.fork_authority_matches(&ready.context.input).await
                {
                    Box::pin(self.commit_fork_navigation(tui, server, operation, handle, *ready))
                        .await?;
                } else {
                    self.fork_failure(&mut ready.context.input, "Fork was not installed. Confirmed source detachments remain detached; drafts are retained. Reopen the source to continue. Close and reopen this TUI to resynchronize unknown detachments; no request was retried.", matching);
                    self.start_navigation_cleanup(
                        operation,
                        handle,
                        ready.started.session.thread_id,
                        if same_epoch { fences } else { Vec::new() },
                        CleanupContinuation::Done,
                        false,
                    );
                }
            }
            (mut stage, ForkOutcome::Terminated) => {
                let source = stage.thread_id();
                let resource = !stage.is_read_only();
                if let Some(recovery) = stage.take_draft() {
                    self.queue_local_draft_recovery(recovery);
                }
                if matching {
                    self.cancel_fork_projection(source);
                    self.chat_widget.add_error_message("Fork preparation terminated unexpectedly. No request was retried; close and reopen this TUI to resynchronize.".into());
                }
                match stage {
                    ForkStage::Detaching { ready, plan } => self.start_navigation_cleanup(
                        operation,
                        handle,
                        ready.started.session.thread_id,
                        if same_epoch {
                            plan.targets.iter().map(|target| target.thread_id).collect()
                        } else {
                            Vec::new()
                        },
                        CleanupContinuation::Done,
                        false,
                    ),
                    _ if resource && same_epoch => {
                        self.retain_navigation_uncertainty(operation, vec![source], false);
                    }
                    _ => {}
                }
            }
            (mut stage, _) if !matching && stage.is_read_only() => {
                if let Some(recovery) = stage.take_draft() {
                    self.queue_local_draft_recovery(recovery);
                }
            }
            _ => unreachable!("fork stage received another stage's receipt"),
        }
        Ok(())
    }
}
