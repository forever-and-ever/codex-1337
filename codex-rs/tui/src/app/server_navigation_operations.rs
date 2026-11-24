//! Threadless navigation shares operation admission, lifecycle and retirement, not a sentinel thread.

use super::ACK_TIMEOUT;
use super::AGENTS_OVERVIEW_VIEW_ID;
use super::AbortOnDropHandle;
use super::AbsolutePathBuf;
use super::AgentNavigationDirection;
use super::App;
use super::AppCommand;
use super::AppRunControl;
use super::AppServerSession;
use super::AppServerStartedThread;
use super::AppServerTarget;
use super::ApprovalsReviewer;
use super::Arc;
use super::AskForApproval;
use super::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
use super::CODEX_CLI_VERSION;
use super::ChatWidget;
use super::ClientRequest;
use super::Config;
use super::ConnectionMutation;
use super::ControlCompletion;
use super::DetachScope;
use super::DraftDisposition;
use super::DraftOrigin;
use super::Duration;
use super::ExitReason;
use super::Feature;
use super::HooksListEntry;
use super::Instant;
use super::LaneState;
use super::LocalDraftRecovery;
use super::Mutex;
use super::NavigationPreview;
use super::OperationLane;
use super::OperationOrigin;
use super::OperationReceipt;
use super::OperationScope;
use super::Path;
use super::PathBuf;
use super::PendingSubmission;
use super::PermissionProfileSelection;
use super::PermissionProfileSnapshot;
use super::ProviderModelChoice;
use super::Result;
use super::RuntimeApprovalPolicyOverride;
use super::RuntimeKeymap;
use super::RuntimePermissionProfileOverride;
use super::RuntimePermissionProfileTurnOverride;
use super::RuntimePolicyOverrideScope;
use super::ServerCompletion;
use super::ServerOperation;
use super::SessionTarget;
use super::SideThreadState;
use super::SubmissionDraft;
use super::SubmissionLane;
use super::SubmissionOutcome;
use super::SubmissionRequest;
use super::ThreadEventAttachment;
use super::ThreadEventStore;
use super::ThreadId;
use super::ThreadInputState;
use super::ThreadSessionState;
use super::ThreadStartSource;
use super::Turn;
use super::TurnStatus;
use super::UserMessage;
use super::UserTurnReplay;
use super::Uuid;
use super::VecDeque;
use super::agent_status_feed;
use super::agents_overview;
use super::apply_managed_new_thread_defaults;
use super::detach;
use super::find_loaded_subagent_threads_for_primary;
use super::navigation_config;
use super::recap;
use super::session_lifecycle;
use super::tui;
use super::ui_requests;

use futures::FutureExt;

#[path = "server_navigation_cleanup.rs"]
mod cleanup;
#[path = "server_directory_navigation.rs"]
mod directory;
#[path = "server_agent_discovery.rs"]
mod discovery;
#[path = "server_fork_navigation.rs"]
mod fork;
#[path = "server_navigation_permissions.rs"]
mod permissions;
use permissions::PermissionAuthority;
#[path = "server_fresh_navigation.rs"]
mod fresh;
#[path = "server_resume_navigation.rs"]
mod resume;
pub(in crate::app) use directory::DirectoryHistory;
#[path = "server_thread_selection.rs"]
mod selection;
#[path = "server_trust_operations.rs"]
pub(super) mod trust;
use cleanup::CleanupContinuation;

use selection::SelectStage;
pub(in crate::app) use selection::SelectionAfter;

#[derive(Default)]
pub(super) struct NavigationOwner {
    current: Option<PendingNavigation>,
    retired: Vec<PendingNavigation>,
    generation: u64,
}

struct PendingNavigation {
    id: Uuid,
    order: u64,
    epoch: u64,
    generation: u64,
    intent: NavigationIntent,
    state: LaneState,
}

enum NavigationIntent {
    Picker,
    Discover(discovery::DiscoveryIntent),
    Select(SelectStage),
    Transitioning,
    PickerStarting,
    ClosePicker,
    Fresh(fresh::FreshStage),
    Cleanup {
        thread_id: ThreadId,
        fences: Vec<ThreadId>,
        next: CleanupContinuation,
    },
    Uncertain(Vec<ThreadId>),
    Resume(resume::ResumeStage),
    Directory(directory::DirectoryStage),
    Fork(fork::ForkStage),
}

pub(crate) struct NavigationCompletion {
    id: Uuid,
    order: u64,
    epoch: u64,
    generation: u64,
    receipt: NavigationReceipt,
}

enum NavigationReceipt {
    Picker(AppServerSession),
    PickerClosed,
    Discovered(discovery::DiscoveryReceipt),
    Selected(selection::SelectionReceipt),
    Fresh(fresh::FreshReceipt),
    Cleaned(Result<(), String>),
    Resumed(resume::ResumeReceipt),
    Directory(directory::DirectoryReceipt),
    Fork(fork::ForkReceipt),
}

impl std::fmt::Debug for NavigationCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NavigationCompletion")
            .field("id", &self.id)
            .field("epoch", &self.epoch)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl NavigationOwner {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn is_empty(&self) -> bool {
        self.current.is_none() && self.retired.is_empty()
    }

    pub(super) fn precedes_barrier(&self, order: u64, epoch: u64) -> bool {
        self.current
            .iter()
            .chain(&self.retired)
            .any(|operation| operation.epoch == epoch && operation.order < order)
    }

    pub(super) fn precedes_thread(&self, thread_id: ThreadId, order: u64, epoch: u64) -> bool {
        self.current.iter().chain(&self.retired).any(|operation| {
            operation.epoch == epoch
                && operation.order < order
                && match &operation.intent {
                    NavigationIntent::Select(stage) => stage.thread_id() == thread_id,
                    NavigationIntent::Fresh(stage) => stage.fences_thread(thread_id),
                    NavigationIntent::Resume(stage) => stage.fences_thread(thread_id),
                    NavigationIntent::Directory(stage) => stage.fences_thread(thread_id),
                    NavigationIntent::Fork(stage) => stage.fences_thread(thread_id),
                    NavigationIntent::Cleanup {
                        thread_id: target,
                        fences,
                        ..
                    } => *target == thread_id || fences.contains(&thread_id),
                    NavigationIntent::Uncertain(fences) => fences.contains(&thread_id),
                    _ => false,
                }
        })
    }
    pub(super) fn retire_epoch(&mut self, epoch: u64) {
        self.retired.retain(|operation| {
            operation.epoch == epoch || !matches!(operation.state, LaneState::Uncertain)
        });
    }

    pub(super) fn preview(&self) -> Option<NavigationPreview> {
        if let Some(operation) = &self.current {
            Some(if matches!(operation.state, LaneState::Uncertain) {
                NavigationPreview::Uncertain
            } else {
                NavigationPreview::Preparing { id: operation.id }
            })
        } else if self
            .retired
            .iter()
            .any(|operation| matches!(operation.state, LaneState::Uncertain))
        {
            Some(NavigationPreview::Uncertain)
        } else if !self.retired.is_empty() {
            Some(NavigationPreview::Cleaning)
        } else {
            None
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "The navigation generation cannot exhaust u64 within one frontend lifetime."
    )]
    pub(super) fn retire_current(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("navigation generation exhausted");
        if let Some(operation) = self.current.take()
            && !matches!(operation.state, LaneState::Queued)
            && !matches!(&operation.intent, NavigationIntent::Fresh(stage) if stage.is_read_only())
            && !matches!(&operation.intent, NavigationIntent::Resume(stage) if stage.is_read_only())
            && !matches!(&operation.intent, NavigationIntent::Directory(stage) if stage.is_read_only())
            && !matches!(&operation.intent, NavigationIntent::Fork(stage) if stage.is_read_only())
            && !matches!(
                operation.intent,
                NavigationIntent::Discover(_)
                    | NavigationIntent::Select(
                        SelectStage::Liveness { .. } | SelectStage::Fallback(_)
                    )
            )
        {
            // Do not abort a resource-producing request: its late session still needs an owner.
            self.retired.push(operation);
        }
    }

    pub(super) fn cancel(&mut self, id: Uuid) -> bool {
        if self
            .current
            .as_ref()
            .is_none_or(|operation| operation.id != id)
        {
            return false;
        }
        self.retire_current();
        true
    }

    #[cfg(test)]
    pub(super) fn has_running(&self) -> bool {
        self.current
            .iter()
            .chain(&self.retired)
            .any(|operation| matches!(operation.state, LaneState::Running(_)))
    }
}

impl App {
    fn recover_pending_navigation_draft(&mut self) {
        let fork_source = self.server_operations.navigation.current.as_ref().and_then(
            |operation| match &operation.intent {
                NavigationIntent::Fork(stage) => Some(stage.thread_id()),
                NavigationIntent::Resume(stage) => stage.pending_fork_source(),
                _ => None,
            },
        );
        if let Some(source) = fork_source {
            self.cancel_fork_projection(source);
        }
        let recovery = self
            .server_operations
            .navigation
            .current
            .as_mut()
            .and_then(|operation| match &mut operation.intent {
                NavigationIntent::Fresh(stage) => stage.take_draft(),
                NavigationIntent::Fork(stage) => stage.take_draft(),
                _ => None,
            });
        if let Some(recovery) = recovery {
            self.queue_local_draft_recovery(recovery);
        }
    }

    pub(in crate::app) fn refresh_retained_thread_input(
        &mut self,
        thread_id: ThreadId,
        input: Option<&ThreadInputState>,
    ) {
        // Retained tasks outlive the channel reset; refresh their fallback at the final handoff.
        if let Some(retained) = self.agents_overview.input_states.get_mut(&thread_id)
            && let Some(input) = input
        {
            *retained = input.clone();
        }
    }

    pub(super) fn cancel_pending_navigation(&mut self, id: Uuid) {
        if self
            .server_operations
            .navigation
            .current
            .as_ref()
            .is_some_and(|operation| operation.id == id)
        {
            self.recover_pending_navigation_draft();
            self.server_operations.navigation.cancel(id);
        }
    }

    pub(in crate::app) fn navigation_generation(&self) -> u64 {
        self.server_operations.navigation.generation()
    }

    #[cfg(test)]
    pub(in crate::app) fn pending_navigation_admission(&self) -> Option<(u64, u64, bool)> {
        self.server_operations
            .navigation
            .current
            .as_ref()
            .map(|operation| {
                (
                    operation.order,
                    operation.epoch,
                    matches!(operation.state, LaneState::Queued),
                )
            })
    }

    fn recovery_draft(message: UserMessage) -> SubmissionDraft {
        SubmissionDraft {
            client_id: Uuid::new_v4().to_string(),
            message,
            origin: DraftOrigin::Message {
                source: crate::chatwidget::UserMessageSource::Prompt,
            },
        }
    }

    fn queue_local_draft_recovery(&mut self, recovery: LocalDraftRecovery) {
        if let Err(error) = self
            .server_operations
            .control_tx
            .send(ServerCompletion::RecoverDraft(recovery))
        {
            let ServerCompletion::RecoverDraft(recovery) = error.0 else {
                unreachable!("local recovery send returned another payload")
            };
            self.chat_widget.recover_submission_drafts(
                vec![Self::recovery_draft(recovery.message)],
                DraftDisposition::Edit,
            );
            self.chat_widget.add_warning_message("The recovery channel is closed. Unsent new-conversation text was recovered into this draft.".into());
        }
    }

    pub(super) async fn recover_local_navigation_draft(&mut self, recovery: LocalDraftRecovery) {
        let draft = Self::recovery_draft(recovery.message);
        match recovery.origin {
            OperationOrigin::Thread { thread_id, store } => {
                self.restore_submission_drafts(
                    thread_id,
                    &store,
                    vec![draft],
                    DraftDisposition::Edit,
                )
                .await;
            }
            OperationOrigin::Connection => self
                .chat_widget
                .recover_submission_drafts(vec![draft], DraftDisposition::Edit),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "App owns the completion receiver throughout local navigation recovery."
    )]
    pub(in crate::app) async fn drain_local_navigation_recovery(&mut self) {
        let mut retained = Vec::new();
        while let Ok(completion) = self.server_operations.control_rx.try_recv() {
            match completion {
                ServerCompletion::RecoverDraft(recovery) => {
                    self.recover_local_navigation_draft(recovery).await;
                }
                completion => retained.push(completion),
            }
        }
        for completion in retained {
            self.server_operations
                .control_tx
                .send(completion)
                .expect("the owned completion receiver is live during recovery");
        }
    }

    pub(in crate::app) fn supersede_pending_navigation(&mut self) {
        self.recover_pending_navigation_draft();
        self.server_operations.navigation.retire_current();
        self.refresh_submission_preview();
    }

    pub(in crate::app) fn enqueue_picker_preparation(&mut self, server: &AppServerSession) {
        self.enqueue_navigation(server, NavigationIntent::Picker);
    }

    #[expect(
        clippy::expect_used,
        reason = "The operation sequence cannot exhaust u64 within one frontend lifetime."
    )]
    fn enqueue_navigation(&mut self, server: &AppServerSession, intent: NavigationIntent) {
        self.supersede_pending_navigation();
        let order = self.server_operations.next_order;
        self.server_operations.next_order =
            order.checked_add(1).expect("operation sequence exhausted");
        self.server_operations.navigation.current = Some(PendingNavigation {
            id: Uuid::new_v4(),
            order,
            epoch: self.server_operations.epoch,
            generation: self.server_operations.navigation.generation,
            intent,
            state: LaneState::Queued,
        });
        self.drive_server_operations(server);
        self.refresh_submission_preview();
    }

    pub(super) fn drive_navigation_operation(&mut self, server: &AppServerSession) {
        let barrier = self
            .server_operations
            .lanes
            .get(&OperationLane::Connection)
            .map(|lane| lane.active.order);
        let Some(mut operation) = self.server_operations.navigation.current.take() else {
            return;
        };
        let thread_target = match &operation.intent {
            NavigationIntent::Select(stage) => Some(stage.thread_id()),
            NavigationIntent::Resume(stage) => Some(stage.thread_id()),
            NavigationIntent::Directory(stage) => Some(stage.thread_id()),
            NavigationIntent::Fork(stage) => Some(stage.thread_id()),
            _ => None,
        };
        let thread_blocked = thread_target.is_some_and(|thread_id| {
            self.server_operations.navigation.precedes_thread(thread_id, operation.order, operation.epoch)
            || self.server_operations.lanes.iter().any(|(lane, earlier)| earlier.active.order < operation.order && matches!(lane, OperationLane::Thread(id) | OperationLane::Realtime(id) if *id == thread_id))
        });
        if !matches!(operation.state, LaneState::Queued)
            || barrier.is_some_and(|order| order < operation.order)
            || thread_blocked
        {
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        if let NavigationIntent::Select(SelectStage::Liveness { thread_id, .. }) = &operation.intent
        {
            let thread_id = *thread_id;
            self.dispatch_selection_liveness(server, &mut operation, thread_id);
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        if matches!(operation.intent, NavigationIntent::Fork(_)) {
            self.dispatch_fork_navigation(server, &mut operation);
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        if matches!(operation.intent, NavigationIntent::Directory(_)) {
            self.dispatch_directory_change(server, &mut operation);
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        if matches!(operation.intent, NavigationIntent::Resume(_)) {
            self.dispatch_session_resume(server, &mut operation);
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        if matches!(operation.intent, NavigationIntent::Fresh(_)) {
            self.dispatch_fresh_defaults(server, &mut operation);
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        if matches!(operation.intent, NavigationIntent::Discover(_)) {
            self.dispatch_discovery(server, &mut operation);
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        assert!(
            matches!(operation.intent, NavigationIntent::Picker),
            "only admitted navigation intents are queued"
        );
        self.spawn_picker_preparation(&mut operation, server.request_view());
        self.server_operations.navigation.current = Some(operation);
    }

    pub(in crate::app) async fn finish_navigation_operation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        completion: NavigationCompletion,
    ) -> Result<AppRunControl> {
        let matching = self
            .server_operations
            .navigation
            .current
            .as_ref()
            .is_some_and(|operation| {
                operation.id == completion.id
                    && operation.epoch == completion.epoch
                    && operation.generation == completion.generation
            })
            && completion.epoch == self.server_operations.epoch;
        let operation = if matching {
            self.server_operations.navigation.current.take()
        } else {
            self.server_operations
                .navigation
                .retired
                .iter()
                .position(|operation| operation.id == completion.id)
                .map(|index| self.server_operations.navigation.retired.remove(index))
        };
        let Some(mut operation) = operation else {
            assert!(
                !matches!(&completion.receipt, NavigationReceipt::Fork(receipt) if receipt.requires_owner()),
                "fork effect lost its owner"
            );
            assert!(
                !matches!(&completion.receipt, NavigationReceipt::Directory(receipt) if receipt.requires_owner()),
                "directory resource receipt lost its owner"
            );
            assert!(
                !matches!(completion.receipt, NavigationReceipt::Cleaned(_)),
                "subscription cleanup lost its owner"
            );
            assert!(
                !matches!(&completion.receipt, NavigationReceipt::Resumed(receipt) if receipt.requires_owner()),
                "resume acquisition lost its owner"
            );
            // Only canceled read-only work can finish without a retained owner.
            assert!(
                !matches!(&completion.receipt, NavigationReceipt::Fresh(receipt) if receipt.requires_owner()),
                "fresh resource-producing receipt lost its owner"
            );
            assert!(
                !matches!(
                    &completion.receipt,
                    NavigationReceipt::Selected(
                        selection::SelectionReceipt::Resumed { .. }
                            | selection::SelectionReceipt::SideDetached(_)
                    )
                ),
                "resource-producing navigation receipt lost its owner"
            );
            if let NavigationReceipt::Picker(session) = completion.receipt {
                self.close_stale_picker_session(
                    completion.id,
                    completion.order,
                    completion.epoch,
                    completion.generation,
                    session,
                );
            }
            return Ok(AppRunControl::Continue);
        };
        match completion.receipt {
            NavigationReceipt::Fork(receipt) => {
                Box::pin(self.finish_fork_navigation(tui, server, operation, matching, receipt))
                    .await?;
            }
            NavigationReceipt::Directory(receipt) => {
                Box::pin(
                    self.finish_directory_navigation(tui, server, operation, matching, receipt),
                )
                .await?;
            }
            NavigationReceipt::Resumed(receipt) => {
                let control = Box::pin(
                    self.finish_resume_navigation(tui, server, operation, matching, receipt),
                )
                .await?;
                self.drive_server_operations(server);
                self.refresh_submission_preview();
                return Ok(control);
            }
            NavigationReceipt::Cleaned(result) => {
                self.finish_navigation_cleanup(server, operation, matching, result);
            }
            NavigationReceipt::Fresh(receipt) => {
                Box::pin(self.finish_fresh_navigation(tui, server, operation, matching, receipt))
                    .await?;
            }
            NavigationReceipt::Discovered(receipt) if matching => {
                self.finish_discovery(tui, server, operation, receipt)
                    .await?;
            }

            NavigationReceipt::Selected(receipt) => {
                Box::pin(
                    self.finish_selection_operation(tui, server, operation, matching, receipt),
                )
                .await?;
            }
            NavigationReceipt::Picker(mut picker) if matching => {
                picker.model_provider_override = self.harness_overrides.model_provider.clone();
                self.drive_server_operations(server);
                self.refresh_submission_preview();
                let selection = Box::pin(
                    crate::resume_picker::run_resume_picker_from_existing_session_with_app_servers(
                        crate::uses_remote_workspace_or_environment(
                            &self.app_server_target,
                            &self.environment_manager,
                        ),
                        tui,
                        &self.config,
                        &self.local_settings,
                        false,
                        false,
                        crate::resume_picker::ResumePickerAppServers::new(
                            vec![picker],
                            server.backend_id(),
                        ),
                        self.primary_thread_id
                            .or(self.current_displayed_thread_id()),
                    ),
                )
                .await;
                match selection {
                    Ok(selection) => {
                        return self
                            .apply_resume_picker_selection(tui, server, selection)
                            .await;
                    }
                    Err(error) => self.add_agents_overview_error(format!(
                        "Failed to open session picker: {error}"
                    )),
                }
            }
            NavigationReceipt::Picker(picker) => {
                operation.intent = NavigationIntent::ClosePicker;
                self.spawn_picker_cleanup(&mut operation, picker);
                self.server_operations.navigation.retired.push(operation);
            }
            NavigationReceipt::Discovered(_) | NavigationReceipt::PickerClosed => {}
        }
        self.drive_server_operations(server);
        self.refresh_submission_preview();
        Ok(AppRunControl::Continue)
    }

    fn close_stale_picker_session(
        &mut self,
        id: Uuid,
        order: u64,
        epoch: u64,
        generation: u64,
        session: AppServerSession,
    ) {
        let mut operation = PendingNavigation {
            id,
            order,
            epoch,
            generation,
            intent: NavigationIntent::ClosePicker,
            state: LaneState::Queued,
        };
        self.spawn_picker_cleanup(&mut operation, session);
        self.server_operations.navigation.retired.push(operation);
    }

    fn spawn_picker_preparation(
        &self,
        operation: &mut PendingNavigation,
        session: AppServerSession,
    ) {
        let completions = self.server_operations.control_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.intent = NavigationIntent::PickerStarting;
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            // Inventory belongs to the captured backend. A request-only view
            // keeps the live event receiver and transport owner in the scheduler.
            let _ = completions.send(ServerCompletion::Navigation(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::Picker(session),
                },
            )));
        })));
    }

    fn spawn_picker_cleanup(&self, operation: &mut PendingNavigation, session: AppServerSession) {
        let completions = self.server_operations.control_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            match std::panic::AssertUnwindSafe(session.shutdown())
                .catch_unwind()
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "failed to close superseded picker session");
                }
                Err(_) => tracing::warn!("superseded picker cleanup task terminated unexpectedly"),
            }
            let _ = completions.send(ServerCompletion::Navigation(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::PickerClosed,
                },
            )));
        })));
    }
}
