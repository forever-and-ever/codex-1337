//! Selection prepares metadata and subscriptions without transferring the visible receiver.

use super::ACK_TIMEOUT;
use super::AbortOnDropHandle;
use super::App;
use super::AppServerSession;
use super::AppServerStartedThread;
use super::Arc;
use super::CleanupContinuation;
use super::Config;
use super::FutureExt;
use super::Instant;
use super::LaneState;
use super::Mutex;
use super::NavigationCompletion;
use super::NavigationIntent;
use super::NavigationReceipt;
use super::PendingNavigation;
use super::Result;
use super::ServerCompletion;
use super::ThreadEventAttachment;
use super::ThreadEventStore;
use super::ThreadId;
use super::ThreadSessionState;
use super::Turn;
use super::Uuid;
use super::detach;
use super::recap;
use super::tui;
use crate::app::ThreadEventChannel;
use crate::app_server_session::session_preparation::PreparedHistory;
use crate::app_server_session::session_preparation::PreparedSession;
use crate::app_server_session::session_preparation::ResumePreparation;
use crate::app_server_session::session_preparation::SubscriptionCustody;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

use crate::app_server_session::session_preparation::RequestPolicy;
use crate::local_settings::LocalSettings;

pub(super) enum SelectStage {
    Liveness {
        thread_id: ThreadId,
        after: SelectionAfter,
    },
    DiscardSide(detach::DetachPlan),
    Resume {
        context: Box<SelectionContext>,
        kind: SelectionKind,
    },
    Fallback(Box<SelectionContext>),
}

impl SelectStage {
    #[expect(
        clippy::expect_used,
        reason = "Side cleanup is admitted with a target thread."
    )]
    pub(super) fn thread_id(&self) -> ThreadId {
        match self {
            Self::Liveness { thread_id, .. } => *thread_id,
            Self::DiscardSide(plan) => plan.origin().expect("side cleanup has a target"),
            Self::Resume { context, .. } | Self::Fallback(context) => context.thread_id,
        }
    }
}

#[derive(Clone)]
pub(in crate::app) enum SelectionAfter {
    SelectOnly,
    Fork,
    DiscardSide(ThreadId),
    SurfaceInactive,
    HideSide { parent: ThreadId },
}

pub(super) struct SelectionContext {
    thread_id: ThreadId,
    pending_profile_on_unavailable_thread: bool,
    cached_session: Option<ThreadSessionState>,
    target: SelectionTarget,
    after: SelectionAfter,
    metadata: Option<crate::app_server_session::session_preparation::ThreadMetadata>,
}

enum SelectionTarget {
    Uncached,
    Cached(Arc<Mutex<ThreadEventStore>>),
}
#[derive(Clone, Copy)]
pub(super) enum SelectionKind {
    Attach,
    Refresh,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplayMode {
    Live,
    Closed,
    AttachFailed,
}
enum SelectedThread {
    Cached {
        replay: ReplayMode,
        verification: Result<Option<String>>,
    },
    Installed(ReplayMode),
    Attached(AppServerStartedThread),
    Refreshed(AppServerStartedThread),
    ReadOnly(Box<api::Thread>),
}

pub(super) enum SelectionReceipt {
    Liveness(Result<crate::app_server_session::session_preparation::ThreadMetadata>),
    Resumed {
        prepared: PreparedSession,
        handle: AppServerRequestHandle,
    },
    Fallback(Box<PreparedHistory>),
    SideDetached(detach::DetachReceipt),
    Terminated,
}

enum SelectionEffect {
    DiscardSide {
        handle: AppServerRequestHandle,
        plan: detach::DetachPlan,
    },
    Liveness {
        thread_id: ThreadId,
        backend_id: crate::backend_id::BackendId,
        handle: AppServerRequestHandle,
    },
    Resume {
        plan: Box<ResumePreparation>,
        handle: AppServerRequestHandle,
    },
    Fallback {
        thread_id: ThreadId,
        backend_id: crate::backend_id::BackendId,
        handle: AppServerRequestHandle,
        config: Box<Config>,
        local_settings: Box<LocalSettings>,
    },
}

impl App {
    pub(in crate::app) fn enqueue_thread_selection(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        after: SelectionAfter,
    ) {
        self.enqueue_navigation(
            server,
            NavigationIntent::Select(SelectStage::Liveness { thread_id, after }),
        );
    }

    pub(super) fn dispatch_selection_liveness(
        &self,
        server: &AppServerSession,
        operation: &mut PendingNavigation,
        thread_id: ThreadId,
    ) {
        self.spawn_selection_effect(
            operation,
            SelectionEffect::Liveness {
                thread_id,
                backend_id: server.backend_id(),
                handle: server.request_handle(),
            },
        );
    }

    fn spawn_selection_effect(&self, operation: &mut PendingNavigation, effect: SelectionEffect) {
        let image_origin = if self.app_server_target.uses_remote_workspace() {
            crate::history_cell::ImagePathOrigin::Remote
        } else {
            crate::history_cell::ImagePathOrigin::Local
        };
        let completions = self.server_operations.control_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            let receipt = Box::pin(std::panic::AssertUnwindSafe(async {
                match effect {
                    SelectionEffect::DiscardSide { handle, plan } => {
                        let receipt = match tokio::time::timeout(
                            ACK_TIMEOUT,
                            detach::send(&handle, id, &plan),
                        )
                        .await
                        {
                            Ok(results) => detach::DetachReceipt::Complete(results),
                            Err(_) => detach::DetachReceipt::BudgetExpired,
                        };
                        SelectionReceipt::SideDetached(receipt)
                    }
                    SelectionEffect::Liveness { thread_id, backend_id, handle } => SelectionReceipt::Liveness(
                        crate::app_server_session::session_preparation::read_thread_with_receipt(
                            RequestPolicy::Interactive,
                            backend_id,
                            &handle,
                            thread_id,
                        )
                        .await,
                    ),
                    SelectionEffect::Resume { plan, handle } => SelectionReceipt::Resumed {
                        prepared: Box::pin(plan.run()).await,
                        handle,
                    },
                    SelectionEffect::Fallback {
                        thread_id,
                        backend_id,
                        handle,
                        config,
                        local_settings,
                    } => SelectionReceipt::Fallback(Box::new(
                        Box::pin(crate::app_server_session::session_preparation::prepare_thread_history(
                            RequestPolicy::Interactive,
                            handle,
                            thread_id,
                            *config,
                            *local_settings,
                            image_origin,
                            backend_id,
                        ))
                        .await,
                    )),
                }
            })
            .catch_unwind())
            .await
            .unwrap_or(SelectionReceipt::Terminated);
            let _ = completions.send(ServerCompletion::Navigation(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::Selected(receipt),
                },
            )));
        })));
    }

    #[expect(
        clippy::expect_used,
        reason = "Side cleanup retains its admitted target until completion."
    )]
    pub(super) async fn finish_selection_operation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        matching: bool,
        receipt: SelectionReceipt,
    ) -> Result<()> {
        let same_epoch = operation.epoch == self.server_operations.epoch;
        let NavigationIntent::Select(stage) =
            std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("selection receipt without selection intent")
        };
        match (stage, receipt) {
            (SelectStage::Liveness { thread_id, after }, SelectionReceipt::Liveness(result))
                if matching =>
            {
                let metadata = result.as_ref().ok().cloned();
                if !self.apply_thread_liveness(result.map(|metadata| metadata.thread), thread_id) {
                    self.add_agents_overview_error(format!(
                        "Agent thread {thread_id} is no longer available."
                    ));
                    return Ok(());
                }
                let mut context = Box::new(self.capture_selection_context(thread_id, after).await);
                context.metadata = metadata;
                self.advance_selection(tui, server, operation, context)
                    .await?;
            }
            (SelectStage::Liveness { .. }, _) if !matching => {}
            (SelectStage::DiscardSide(plan), SelectionReceipt::SideDetached(receipt)) => {
                if same_epoch {
                    let unknown = match &receipt {
                        detach::DetachReceipt::BudgetExpired => true,
                        detach::DetachReceipt::Complete(results) => {
                            results.iter().any(|result| result.result.is_err())
                        }
                    };
                    let thread_id = plan.origin().expect("side cleanup has a target");
                    self.finish_detach_plan(&plan, receipt).await;
                    if !unknown {
                        for pane in self.sidebars.values_mut() {
                            if let Some(creation) = &mut pane.creation
                                && let crate::app::sidebar::CreationState::Failed {
                                    cleanup, ..
                                } = &mut creation.state
                                && *cleanup == Some(thread_id)
                            {
                                *cleanup = None;
                            }
                        }
                    }
                    if unknown {
                        self.add_agents_overview_error(format!("Closing side conversation {thread_id} was not confirmed. Close and reopen this TUI to resynchronize; the earlier request may still complete."));
                        self.retain_navigation_uncertainty(operation, vec![thread_id], false);
                    }
                }
            }
            (
                SelectStage::Resume { context, kind },
                SelectionReceipt::Resumed { prepared, handle },
            ) => {
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
                let matching = matching && self.selection_target_is_current(&context);
                if !matching {
                    if matches!(kind, SelectionKind::Attach)
                        && let SubscriptionCustody::Subscribed(thread_id) = custody
                    {
                        self.start_navigation_cleanup(
                            operation,
                            handle,
                            thread_id,
                            Vec::new(),
                            CleanupContinuation::Done,
                            false,
                        );
                    } else if same_epoch && let SubscriptionCustody::Uncertain(thread_id) = custody
                    {
                        self.retain_navigation_uncertainty(operation, vec![thread_id], false);
                    }
                    return Ok(());
                }
                match result {
                    Ok(started) => {
                        if started.can_accept_text_input {
                            self.agent_navigation
                                .text_input_threads
                                .insert(context.thread_id);
                        } else {
                            self.agent_navigation
                                .text_input_threads
                                .remove(&context.thread_id);
                        }
                        if started.blocks_direct_input {
                            self.agent_navigation.mark_parent_owned(context.thread_id);
                        }
                        let selected = match kind {
                            SelectionKind::Attach => SelectedThread::Attached(started),
                            SelectionKind::Refresh => SelectedThread::Refreshed(started),
                        };
                        self.commit_agent_selection(tui, server, *context, selected)
                            .await?;
                    }
                    Err(error) => {
                        if let Some(required) = crate::app::sign_in_required::from_report(&error) {
                            self.ensure_thread_channel(context.thread_id)
                                .require_sign_in(required);
                        } else if matches!(kind, SelectionKind::Attach)
                            && matches!(custody, SubscriptionCustody::None)
                            && crate::app_server_session::is_active_writer_error(&error)
                        {
                            let channel = self.ensure_thread_channel(context.thread_id);
                            if !channel.is_archived() {
                                channel.mark_external_writer();
                            }
                        }
                        match custody {
                            SubscriptionCustody::Uncertain(thread_id) => {
                                self.add_agents_overview_error(format!("Conversation attachment outcome is unknown: {error}. No resume was retried; close and reopen this TUI to resynchronize. The earlier request may still complete."));
                                self.retain_navigation_uncertainty(
                                    operation,
                                    vec![thread_id],
                                    true,
                                );
                            }
                            SubscriptionCustody::Subscribed(thread_id)
                                if matches!(kind, SelectionKind::Attach) =>
                            {
                                self.start_navigation_cleanup(
                                    operation,
                                    handle,
                                    thread_id,
                                    Vec::new(),
                                    CleanupContinuation::SelectionFallback(context),
                                    true,
                                );
                            }
                            _ if matches!(kind, SelectionKind::Refresh) => {
                                tracing::warn!(%error, "failed to refresh inferred thread session before replay");
                                self.commit_agent_selection(
                                    tui,
                                    server,
                                    *context,
                                    SelectedThread::Cached {
                                        replay: ReplayMode::Live,
                                        verification: Err(error),
                                    },
                                )
                                .await?;
                            }
                            _ => self.start_selection_fallback(server, operation, context),
                        }
                    }
                }
            }
            (SelectStage::Fallback(context), SelectionReceipt::Fallback(prepared)) => {
                if same_epoch {
                    let result =
                        prepared
                            .apply(server)
                            .and_then(|hydrated| match hydrated.history {
                                Ok(()) => Ok(hydrated.thread),
                                Err(error)
                                    if Self::can_fallback_from_include_turns_error(&error) =>
                                {
                                    Ok(hydrated.thread)
                                }
                                Err(error) => Err(error),
                            });
                    if matching && self.selection_target_is_current(&context) {
                        match result {
                            Ok(thread) if !thread.ephemeral && !thread.turns.is_empty() => {
                                self.commit_agent_selection(
                                    tui,
                                    server,
                                    *context,
                                    SelectedThread::ReadOnly(Box::new(thread)),
                                )
                                .await?;
                            }
                            result => {
                                if self.thread_event_channels.contains_key(&context.thread_id) {
                                    self.commit_agent_selection(
                                        tui,
                                        server,
                                        *context,
                                        SelectedThread::Cached {
                                            replay: ReplayMode::AttachFailed,
                                            verification: Ok(None),
                                        },
                                    )
                                    .await?;
                                } else {
                                    let error = match result {
                                        Err(error) => error.to_string(),
                                        _ => format!(
                                            "Agent thread {} is not yet available for replay or live attach.",
                                            context.thread_id
                                        ),
                                    };
                                    self.add_agents_overview_error(error);
                                }
                            }
                        }
                    }
                }
            }
            (stage, SelectionReceipt::Terminated) => {
                let thread_id = stage.thread_id();
                self.add_agents_overview_error("Conversation preparation terminated unexpectedly. No request was retried; close and reopen this TUI to resynchronize.".into());
                if same_epoch
                    && matches!(
                        stage,
                        SelectStage::Resume { .. } | SelectStage::DiscardSide(_)
                    )
                {
                    self.retain_navigation_uncertainty(operation, vec![thread_id], matching);
                }
            }
            _ => unreachable!("selection stage received a different family's receipt"),
        }
        Ok(())
    }

    async fn capture_selection_context(
        &self,
        thread_id: ThreadId,
        after: SelectionAfter,
    ) -> SelectionContext {
        let unavailable = self.thread_unavailable(thread_id);
        let target = match self.thread_event_channels.get(&thread_id) {
            Some(channel) => SelectionTarget::Cached(Arc::clone(&channel.store)),
            None => SelectionTarget::Uncached,
        };
        let cached_session = if unavailable {
            match &target {
                SelectionTarget::Cached(store) => store.lock().await.session.clone(),
                SelectionTarget::Uncached => None,
            }
        } else {
            None
        };
        SelectionContext {
            thread_id,
            pending_profile_on_unavailable_thread: unavailable
                && self.pending_server_profiles.contains_key(&thread_id),
            cached_session,
            target,
            after,
            metadata: None,
        }
    }

    fn selection_target_is_current(&self, context: &SelectionContext) -> bool {
        match &context.target {
            SelectionTarget::Uncached => true,
            SelectionTarget::Cached(store) => self
                .thread_event_channels
                .get(&context.thread_id)
                .is_some_and(|channel| Arc::ptr_eq(&channel.store, store)),
        }
    }

    async fn advance_selection(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        mut context: Box<SelectionContext>,
    ) -> Result<()> {
        let thread_id = context.thread_id;
        let closed = self
            .agent_navigation
            .get(&thread_id)
            .is_some_and(|entry| entry.is_closed);
        let kind = if self.should_attach_live_thread_for_selection(thread_id) {
            Some(SelectionKind::Attach)
        } else if let Some(channel) = self.thread_event_channels.get(&thread_id) {
            let store = channel.store.lock().await;
            self.should_refresh_thread_session(thread_id, closed, store.session.as_ref())
                .then_some(SelectionKind::Refresh)
        } else {
            None
        };
        if let Some(kind) = kind {
            let mut plan = server.prepare_resume_thread(
                RequestPolicy::Interactive,
                &self.local_settings,
                self.config.clone(),
                thread_id,
                crate::app_server_session::ResumeModelSettings::PreserveExistingThread,
                crate::resume_permissions::ResumePermissions::default(),
            );
            if let Some(metadata) = context.metadata.take() {
                plan = plan.with_metadata(metadata);
            }
            operation.intent = NavigationIntent::Select(SelectStage::Resume { context, kind });
            self.spawn_selection_effect(
                &mut operation,
                SelectionEffect::Resume {
                    plan: Box::new(plan),
                    handle: server.request_handle(),
                },
            );
            self.server_operations.navigation.current = Some(operation);
        } else if !self.thread_event_channels.contains_key(&thread_id) {
            self.add_agents_overview_error(format!(
                "Agent thread {thread_id} is no longer available."
            ));
        } else {
            self.commit_agent_selection(
                tui,
                server,
                *context,
                SelectedThread::Cached {
                    replay: if closed {
                        ReplayMode::Closed
                    } else {
                        ReplayMode::Live
                    },
                    verification: Ok(None),
                },
            )
            .await?;
        }
        Ok(())
    }

    pub(super) fn start_selection_fallback(
        &mut self,
        server: &AppServerSession,
        mut operation: PendingNavigation,
        context: Box<SelectionContext>,
    ) {
        let thread_id = context.thread_id;
        operation.intent = NavigationIntent::Select(SelectStage::Fallback(context));
        self.spawn_selection_effect(
            &mut operation,
            SelectionEffect::Fallback {
                thread_id,
                backend_id: server.backend_id(),
                handle: server.request_handle(),
                config: Box::new(self.config.clone()),
                local_settings: Box::new(self.local_settings.clone()),
            },
        );
        self.server_operations.navigation.current = Some(operation);
    }

    pub(in crate::app) async fn install_selected_snapshot(
        &mut self,
        thread_id: ThreadId,
        session: ThreadSessionState,
        turns: Vec<Turn>,
        live_attached: bool,
    ) {
        let sign_in_required = self
            .thread_event_channels
            .get(&thread_id)
            .and_then(|channel| match channel.attachment() {
                ThreadEventAttachment::SignInRequired(required) => Some(required),
                _ => None,
            });
        let attachment = self
            .thread_event_channels
            .get(&thread_id)
            .map(ThreadEventChannel::attachment);
        if self.active_thread_id == Some(thread_id) {
            self.store_active_thread_receiver().await;
        }
        self.agents_overview.activity.remove(&thread_id);
        let epoch = self.reconnect.connection_epoch;
        let channel = self.ensure_thread_channel(thread_id);
        if live_attached {
            channel.mark_live();
        } else if let Some(required) = sign_in_required {
            channel.require_sign_in(required);
        } else {
            match attachment {
                Some(ThreadEventAttachment::Archived) => channel.mark_archived(),
                Some(ThreadEventAttachment::ExternalWriter) => channel.mark_external_writer(),
                _ => channel.mark_replay_only(),
            }
        }
        let mut store = channel.store.lock().await;
        store.at_connection_epoch(epoch).set_session(session, turns);
        store.rebase_buffer_after_session_refresh();
    }

    pub(in crate::app) async fn select_installed_side_locally(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        live: bool,
    ) -> Result<()> {
        self.supersede_pending_navigation();
        let context = self
            .capture_selection_context(thread_id, SelectionAfter::SelectOnly)
            .await;
        self.commit_agent_selection(
            tui,
            server,
            context,
            SelectedThread::Installed(if live {
                ReplayMode::Live
            } else {
                ReplayMode::AttachFailed
            }),
        )
        .await
    }

    pub(in crate::app) async fn select_tracked_side_locally(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        after: SelectionAfter,
    ) -> Result<()> {
        self.supersede_pending_navigation();
        let context = self.capture_selection_context(thread_id, after).await;
        self.commit_agent_selection(
            tui,
            server,
            context,
            SelectedThread::Cached {
                replay: ReplayMode::Live,
                verification: Ok(None),
            },
        )
        .await
    }

    pub(in crate::app) async fn finish_selection_after(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        after: SelectionAfter,
    ) -> Result<()> {
        match after {
            SelectionAfter::SelectOnly => {}
            SelectionAfter::HideSide { parent } => {
                if self.current_displayed_thread_id() == Some(parent) {
                    self.sidebar_mut().hide();
                    tui.frame_requester().schedule_frame();
                    self.focus_returned_parent_composer(parent).await?;
                }
            }
            SelectionAfter::Fork => {
                // The continuation lease ends here; admission installs the fork's lease.
                self.chat_widget.fork_in_progress = false;
                self.fork_current_thread(tui, server, None, Vec::new(), true);
            }
            SelectionAfter::DiscardSide(thread_id) => {
                self.discard_side_thread_in_background(server, thread_id)
                    .await;
                self.surface_pending_inactive_thread_interactive_requests()
                    .await?;
            }
            SelectionAfter::SurfaceInactive => {
                if self.active_side_parent_thread_id().is_none() {
                    self.surface_pending_inactive_thread_interactive_requests()
                        .await?;
                }
            }
        }
        Ok(())
    }

    #[expect(
        clippy::expect_used,
        reason = "The operation sequence cannot exhaust u64 within one frontend lifetime."
    )]
    pub(in crate::app) async fn enqueue_side_cleanup(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
    ) {
        let channel = self.ensure_thread_channel(thread_id);
        let store = Arc::clone(&channel.store);
        let replay_only = channel.attachment() == ThreadEventAttachment::ReplayOnly;
        let turn_id = self
            .active_turn_id_for_thread(thread_id)
            .await
            .unwrap_or_default();
        let failed_creation = self.sidebars.values().any(|pane| pane.creation.as_ref().is_some_and(|creation| matches!(creation.state, crate::app::sidebar::CreationState::Failed { cleanup: Some(target), .. } if target == thread_id)));
        if self.server_operations.navigation.retired.iter().any(|operation| matches!(&operation.intent, NavigationIntent::Select(SelectStage::DiscardSide(plan)) if plan.origin() == Some(thread_id))) { return; }
        let plan = detach::DetachPlan {
            targets: vec![detach::DetachTarget {
                thread_id,
                store,
                kind: if failed_creation {
                    detach::DetachKind::FailedSide
                } else {
                    detach::DetachKind::Side {
                        turn_id,
                        replay_only,
                    }
                },
            }],
            voice: None,
        };
        let order = self.server_operations.next_order;
        self.server_operations.next_order =
            order.checked_add(1).expect("operation sequence exhausted");
        let mut operation = PendingNavigation {
            id: Uuid::new_v4(),
            order,
            epoch: self.server_operations.epoch,
            generation: self.server_operations.navigation.generation,
            intent: NavigationIntent::Select(SelectStage::DiscardSide(plan.clone())),
            state: LaneState::Queued,
        };
        self.spawn_selection_effect(
            &mut operation,
            SelectionEffect::DiscardSide {
                handle: server.request_handle(),
                plan,
            },
        );
        self.server_operations.navigation.retired.push(operation);
        self.refresh_submission_preview();
    }

    async fn commit_agent_selection(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        context: SelectionContext,
        selected: SelectedThread,
    ) -> Result<()> {
        let settings_authority = match &selected {
            SelectedThread::Cached { .. } | SelectedThread::ReadOnly(_) => {
                crate::chatwidget::ThreadInputSettingsAuthority::Retained
            }
            SelectedThread::Installed(_)
            | SelectedThread::Attached(_)
            | SelectedThread::Refreshed(_) => {
                crate::chatwidget::ThreadInputSettingsAuthority::Session {
                    accepted_mode: None,
                }
            }
        };
        let SelectionContext {
            thread_id,
            pending_profile_on_unavailable_thread,
            cached_session,
            after,
            ..
        } = context;
        let (mut is_replay_only, mut attached_replay_only, verification, refreshed) = match selected
        {
            SelectedThread::Cached {
                replay,
                verification,
            } => (
                replay != ReplayMode::Live,
                replay == ReplayMode::AttachFailed,
                verification,
                None,
            ),
            SelectedThread::Installed(replay) => (
                replay != ReplayMode::Live,
                replay == ReplayMode::AttachFailed,
                Ok(None),
                None,
            ),
            SelectedThread::Attached(started) => {
                let notice = started.history_notice.clone();
                self.install_selected_snapshot(thread_id, started.session, started.turns, true)
                    .await;
                (false, false, Ok(notice), None)
            }
            SelectedThread::Refreshed(started) => (false, false, Ok(None), Some(started)),
            SelectedThread::ReadOnly(thread) => {
                let turns = thread.turns.clone();
                let cwd = crate::app_server_session::decode_thread_cwd(
                    &thread.cwd,
                    app_server.path_convention(),
                )
                .ok();
                let mut session = self
                    .session_state_for_thread_read(thread_id, &thread, cwd)
                    .await;
                if let Some(channel) = self.thread_event_channels.get(&thread_id)
                    && let Some(cached) = channel.store.lock().await.session.as_ref()
                {
                    session.approval_policy = cached.approval_policy;
                    session.permission_profile = cached.permission_profile.clone();
                    session.active_permission_profile = cached.active_permission_profile.clone();
                    session.approvals_reviewer = cached.approvals_reviewer;
                }
                session.model.clear();
                self.install_selected_snapshot(thread_id, session, turns, false)
                    .await;
                (true, true, Ok(None), None)
            }
        };
        self.agent_navigation.explicit_attachments.insert(thread_id);
        let previous_thread_id = self.active_thread_id;
        // Notifications already routed to the active channel must reach the old
        // widget before its transcript state is transferred. Events routed after
        // deactivation are retained separately for that thread.
        if let Some(mut receiver) = self.active_thread_rx.take() {
            while let Ok(event) = receiver.try_recv() {
                self.handle_thread_event_now_recovering_file_changes(event)
                    .await;
            }
            self.active_thread_rx = Some(receiver);
        }
        self.store_active_thread_receiver().await;
        self.active_thread_id = None;
        let Some((receiver, mut snapshot)) = self.activate_thread_for_replay(thread_id).await
        else {
            self.add_agents_overview_error(format!("Agent thread {thread_id} is already active."));
            if let Some(previous_thread_id) = previous_thread_id {
                self.activate_thread_channel(previous_thread_id).await;
            }
            return Ok(());
        };

        let session_verification = match refreshed {
            Some(mut started) => {
                let notice = started.history_notice.take();
                self.apply_refreshed_snapshot_thread(thread_id, started, &mut snapshot)
                    .await;
                Ok(notice)
            }
            None => verification,
        };
        let session_verified = session_verification.is_ok();
        if !is_replay_only
            && !self.side_threads.contains_key(&thread_id)
            && snapshot.session.as_ref().is_some_and(|session| {
                session
                    .active_permission_profile
                    .as_ref()
                    .is_some_and(|profile| !profile.id.starts_with(':'))
            })
            && !session_verified
        {
            is_replay_only = true;
            attached_replay_only = true;
        }
        if pending_profile_on_unavailable_thread && !is_replay_only && session_verified {
            self.pending_server_profiles.remove(&thread_id);
        }
        // Refreshing can merge restored turns into the store, so recap progress must be read only
        // after the refresh while the activated thread channel is still retained.
        let Some(channel) = self.thread_event_channels.get(&thread_id) else {
            self.add_agents_overview_error(format!(
                "Agent thread {thread_id} is no longer available."
            ));
            return Ok(());
        };
        let recap_progress = {
            let mut store = channel.store.lock().await;
            if let (Some(cached), Some(session)) =
                (cached_session.as_ref(), snapshot.session.as_mut())
            {
                if !pending_profile_on_unavailable_thread {
                    self.restore_runtime_permissions(session, cached);
                }
                store.session = Some(session.clone());
                if !is_replay_only && self.primary_thread_id == Some(thread_id) {
                    self.primary_session_configured = Some(session.clone());
                }
            }
            store.recap_progress()
        };
        snapshot.input_state = snapshot
            .input_state
            .or(self.agents_overview.input_states.remove(&thread_id));

        self.active_thread_id = Some(thread_id);
        self.active_thread_rx = Some(receiver);

        self.recap.note_focus_gained();
        self.recap = recap::RecapState::default();

        if !tui.is_terminal_focused() {
            self.recap.note_focus_lost(Instant::now());
        }
        let now = Instant::now();
        self.recap.seed_from_progress(recap_progress, now);
        self.schedule_recap_check(thread_id, now);

        self.render_thread_snapshot(
            tui,
            app_server,
            thread_id,
            snapshot,
            !is_replay_only,
            settings_authority,
        )?;
        if self.side_retention_enabled(tui) {
            if self.side_threads.contains_key(&thread_id) {
                self.sidebar_mut().show(
                    crate::app::sidebar::SidebarContent::Chat(thread_id),
                    crate::app::sidebar::PaneFocus::Sidebar,
                );
            } else if let Some(crate::app::sidebar::SidebarContent::Chat(child)) = self
                .sidebar()
                .and_then(crate::app::sidebar::SidebarPane::visible_content)
            {
                self.sidebar_mut().show(
                    crate::app::sidebar::SidebarContent::Chat(child),
                    crate::app::sidebar::PaneFocus::Main,
                );
            }
        }
        if let Ok(Some(notice)) = session_verification {
            self.chat_widget.add_info_message(notice, /*hint*/ None);
        }
        if is_replay_only
            && self
                .thread_event_channels
                .get(&thread_id)
                .is_none_or(|channel| channel.attachment() != ThreadEventAttachment::ExternalWriter)
        {
            self.chat_widget.pause_unavailable_thread();
            let message = if let Some(ThreadEventAttachment::SignInRequired(required)) = self
                .thread_event_channels
                .get(&thread_id)
                .map(ThreadEventChannel::attachment)
            {
                crate::app::sign_in_required::message(&required)
            } else if attached_replay_only {
                format!(
                    "Agent thread {thread_id} could not be resumed live. Replaying saved transcript."
                )
            } else {
                format!("Agent thread {thread_id} is closed. Replaying saved transcript.")
            };
            self.chat_widget.add_info_message(message, /*hint*/ None);
        }
        self.refresh_pending_thread_approvals().await;
        self.sync_frontend_requests();
        self.finish_selection_after(tui, app_server, after).await?;

        Ok(())
    }
}
