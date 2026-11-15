//! Destructive task actions retain the connection barrier through reconciliation and presentation.

use super::App;
use super::AppEvent;
use super::AppRunControl;
use super::AppServerSession;
use super::AppServerTarget;
use super::Arc;
use super::ChatWidget;
use super::ClientRequest;
use super::ConnectionMutation;
use super::ControlCompletion;
use super::DetachScope;
use super::DraftDisposition;
use super::HashSet;
use super::LaneState;
use super::OperationLane;
use super::OperationReceipt;
use super::OperationScope;
use super::Result;
use super::ServerOperation;
use super::SubmissionLane;
use super::SubmissionOutcome;
use super::SubmissionRequest;
use super::ThreadEventAttachment;
use super::ThreadId;
use super::TypedRequestError;
use super::Uuid;
use super::detach;
use super::tui;
use crate::app_event::AgentsOverviewAction;
use crate::app_server_session::ThreadDeletion;
use crate::app_server_session::ThreadDeletionError;
use crate::app_server_session::typed_requests::SessionRpc;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;
use codex_app_server_protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
#[cfg(test)]
use std::time::Duration;

#[derive(Clone, Copy)]
enum RemovalSource {
    Current,
    Overview,
}

#[derive(Clone)]
enum RemovalMutation {
    Archive,
    Delete(ThreadDeletion),
}

impl RemovalMutation {
    fn action(&self) -> AgentsOverviewAction {
        match self {
            Self::Archive => AgentsOverviewAction::Archive,
            Self::Delete(_) => AgentsOverviewAction::Delete,
        }
    }
}

#[derive(Clone)]
pub(in crate::app) struct RemovalRequest {
    thread_id: ThreadId,
    mutation: RemovalMutation,
    source: RemovalSource,
    generation: u64,
}

#[derive(Clone)]
pub(in crate::app) struct RemovalTarget {
    request: RemovalRequest,
    primary: Option<ThreadId>,
}

#[derive(Clone)]
pub(in crate::app) enum RemovalOperation {
    Inspect {
        request: RemovalRequest,
        primary: Option<ThreadId>,
        voice: Option<ThreadId>,
    },
    Before {
        target: RemovalTarget,
        plan: detach::DetachPlan,
    },
    Mutate(RemovalTarget),
    Reconcile {
        target: RemovalTarget,
        failure: Arc<TypedRequestError>,
    },
    After {
        target: RemovalTarget,
        failure: Option<Arc<TypedRequestError>>,
        plan: detach::DetachPlan,
    },
}

impl super::ServerOperations {
    /// Presentation is derived from the admitted lifecycle operation, never a second owner.
    pub(in crate::app) fn overview_removal(&self) -> Option<AgentsOverviewAction> {
        let lane = self.lanes.get(&OperationLane::Connection)?;
        std::iter::once(&lane.active)
            .chain(&lane.waiting)
            .find_map(|pending| {
                let SubmissionRequest::Operation(operation) = &pending.request else {
                    return None;
                };
                match &**operation {
                    ServerOperation::Connection(ConnectionMutation::Removal(removal)) => {
                        Some(removal.request().mutation.action())
                    }
                    _ => None,
                }
            })
    }
}

#[derive(Debug)]
pub(in crate::app) enum RemovalReceipt {
    Inspected {
        primary: Option<ThreadId>,
        voice: Option<ThreadId>,
    },
    Detached(detach::DetachReceipt),
    Mutated(Result<(), Arc<TypedRequestError>>),
    Refused(String),
    PrimaryLoaded(bool),
    Finished,
}

enum DetachCertainty {
    Confirmed,
    Rejected,
    Uncertain,
}

fn detach_certainty(receipt: &detach::DetachReceipt) -> DetachCertainty {
    let detach::DetachReceipt::Complete(results) = receipt else {
        return DetachCertainty::Uncertain;
    };
    let mut certainty = DetachCertainty::Confirmed;
    for result in results {
        match &result.result {
            Ok(()) => {}
            Err(TypedRequestError::Server { .. }) => certainty = DetachCertainty::Rejected,
            Err(_) => return DetachCertainty::Uncertain,
        }
    }
    certainty
}

impl RemovalOperation {
    pub(super) fn is_read_only(&self) -> bool {
        matches!(self, Self::Inspect { .. } | Self::Reconcile { .. })
    }

    pub(super) fn cancellable_before_send(&self) -> bool {
        matches!(self, Self::Inspect { .. })
    }

    fn request(&self) -> &RemovalRequest {
        match self {
            Self::Inspect { request, .. } => request,
            Self::Before { target, .. }
            | Self::Mutate(target)
            | Self::Reconcile { target, .. }
            | Self::After { target, .. } => &target.request,
        }
    }

    pub(super) fn label(&self) -> &'static str {
        match self.request().mutation.action() {
            AgentsOverviewAction::Archive => "Archiving task",
            AgentsOverviewAction::Delete => "Deleting task",
        }
    }
}

async fn belongs_to(
    handle: &AppServerRequestHandle,
    id: Uuid,
    thread_id: ThreadId,
    root: ThreadId,
) -> Result<bool, TypedRequestError> {
    if thread_id == root {
        return Ok(true);
    }
    let response: api::ThreadReadResponse = handle
        .thread_read(
            api::RequestId::String(format!("tui-removal-{id}-read-{thread_id}")),
            api::ThreadReadParams {
                thread_id: thread_id.to_string(),
                include_turns: false,
            },
        )
        .await?;
    Ok(response.thread.session_id == root.to_string())
}

#[expect(
    clippy::expect_used,
    reason = "Removal reconciliation is constructed with the affected primary thread."
)]
pub(super) async fn send(
    handle: AppServerRequestHandle,
    id: Uuid,
    operation: RemovalOperation,
) -> Result<OperationReceipt, TypedRequestError> {
    let receipt = match operation {
        RemovalOperation::Inspect {
            request,
            primary,
            voice,
        } => {
            let primary = match primary {
                Some(primary) if belongs_to(&handle, id, primary, request.thread_id).await? => {
                    Some(primary)
                }
                _ => None,
            };
            let voice = match voice {
                Some(voice) if belongs_to(&handle, id, voice, request.thread_id).await? => {
                    Some(voice)
                }
                _ => None,
            };
            RemovalReceipt::Inspected { primary, voice }
        }
        RemovalOperation::Before { plan, .. } | RemovalOperation::After { plan, .. } => {
            RemovalReceipt::Detached(detach::DetachReceipt::Complete(
                detach::send(&handle, id, &plan).await,
            ))
        }
        RemovalOperation::Mutate(target) => {
            let request_id = api::RequestId::String(format!("tui-removal-{id}-mutate"));
            let result = match target.request.mutation {
                RemovalMutation::Archive => handle
                    .request_typed::<api::ThreadArchiveResponse>(ClientRequest::ThreadArchive {
                        request_id,
                        params: api::ThreadArchiveParams {
                            thread_id: target.request.thread_id.to_string(),
                        },
                    })
                    .await
                    .map(|_| ())
                    .map_err(ThreadDeletionError::Request),
                RemovalMutation::Delete(deletion) => deletion.run(request_id).await.map(|_| ()),
            };
            // A definite rejection can still follow server-side shutdown. Reconcile that
            // fact before restoring the old view. A lost reply is never a safe retry.
            match result {
                Err(ThreadDeletionError::Preflight(error)) => {
                    RemovalReceipt::Refused(error.to_string())
                }
                Err(ThreadDeletionError::Request(error @ TypedRequestError::Server { .. })) => {
                    RemovalReceipt::Mutated(Err(Arc::new(error)))
                }
                Err(ThreadDeletionError::Request(error)) => return Err(error),
                Ok(()) => RemovalReceipt::Mutated(Ok(())),
            }
        }
        RemovalOperation::Reconcile { target, .. } => {
            let primary = target
                .primary
                .expect("reconciliation has an affected primary");
            let mut cursor = None;
            let loaded = loop {
                let page: api::ThreadLoadedListResponse = handle
                    .thread_loaded_list(
                        api::RequestId::String(format!("tui-removal-{id}-loaded")),
                        api::ThreadLoadedListParams {
                            cursor,
                            limit: None,
                        },
                    )
                    .await?;
                if page.data.contains(&primary.to_string()) {
                    break true;
                }
                let Some(next) = page.next_cursor else {
                    break false;
                };
                cursor = Some(next);
            };
            RemovalReceipt::PrimaryLoaded(loaded)
        }
    };
    Ok(OperationReceipt::Removal(receipt))
}

impl App {
    pub(in crate::app) fn enqueue_current_thread_removal(
        &mut self,
        server: &AppServerSession,
        action: AgentsOverviewAction,
    ) -> AppRunControl {
        let verb = match action {
            AgentsOverviewAction::Archive => "archived",
            AgentsOverviewAction::Delete => "deleted",
        };
        let Some(thread_id) = self.active_thread_id.or(self.chat_widget.thread_id()) else {
            self.chat_widget
                .add_error_message(format!("A thread must start before it can be {verb}."));
            return AppRunControl::Continue;
        };
        if self.side_threads.contains_key(&thread_id) {
            self.chat_widget.add_error_message(format!(
                "Side conversations cannot be {verb} directly. Press Ctrl+C to return to the main thread first."
            ));
            return AppRunControl::Continue;
        }
        self.enqueue_removal(server, thread_id, action, RemovalSource::Current);
        AppRunControl::Continue
    }

    pub(in crate::app) fn enqueue_overview_removal(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        action: AgentsOverviewAction,
    ) {
        if !self.windows_sandbox_blocks_thread_switch() {
            self.enqueue_removal(server, thread_id, action, RemovalSource::Overview);
        }
    }

    fn enqueue_removal(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        action: AgentsOverviewAction,
        source: RemovalSource,
    ) {
        self.supersede_pending_navigation();
        let request = RemovalRequest {
            thread_id,
            mutation: match action {
                AgentsOverviewAction::Archive => RemovalMutation::Archive,
                AgentsOverviewAction::Delete => {
                    RemovalMutation::Delete(server.prepare_thread_delete(thread_id))
                }
            },
            source,
            generation: self.server_operations.navigation.generation(),
        };
        let operation = RemovalOperation::Inspect {
            request,
            primary: self.primary_thread_id,
            voice: self.voice_owner_thread_id(),
        };
        self.enqueue_connection_operation(
            server,
            ServerOperation::Connection(ConnectionMutation::Removal(operation)),
        );
    }

    pub(super) fn owns_removal_completion(&self, completion: &ControlCompletion) -> bool {
        !self.reconnect.offline
            && matches!(completion.scope, OperationScope::Connection)
            && completion.epoch == self.server_operations.epoch
            && self.server_operations.lanes.get(&OperationLane::Connection).is_some_and(|lane| {
                lane.active.id == completion.id
                    && matches!(&lane.active.request, SubmissionRequest::Operation(operation)
                        if matches!(&**operation, ServerOperation::Connection(ConnectionMutation::Removal(_))))
            })
    }

    fn continue_removal(
        &mut self,
        server: &AppServerSession,
        mut lane: SubmissionLane,
        operation: RemovalOperation,
    ) {
        lane.active.request = SubmissionRequest::Operation(Box::new(ServerOperation::Connection(
            ConnectionMutation::Removal(operation),
        )));
        self.start_submission(server, lane.active, lane.waiting);
        self.refresh_submission_preview();
    }

    fn removal_view_matches(&self, target: &RemovalTarget) -> bool {
        self.server_operations.navigation.generation() == target.request.generation
            && match target.request.source {
                RemovalSource::Current => {
                    self.current_displayed_thread_id() == Some(target.request.thread_id)
                }
                RemovalSource::Overview => {
                    target.primary.is_some() && self.primary_thread_id == target.primary
                }
            }
    }

    fn present_removal_failure(&mut self, target: &RemovalTarget, error: &impl std::fmt::Display) {
        match target.request.source {
            RemovalSource::Overview => {
                self.show_thread_removal_failure(target.request.mutation.action(), error);
            }
            RemovalSource::Current => {
                let verb = match target.request.mutation.action() {
                    AgentsOverviewAction::Archive => "archive",
                    AgentsOverviewAction::Delete => "delete",
                };
                self.chat_widget
                    .add_error_message(format!("Failed to {verb} current thread: {error}"));
            }
        }
    }

    fn pause_removed_primary(&mut self, target: &RemovalTarget) {
        if let Some(primary) = target.primary {
            if let Some(channel) = self.thread_event_channels.get_mut(&primary) {
                channel.mark_replay_only();
            }
            if self.chat_widget.thread_id() == Some(primary) {
                self.chat_widget.pause_unavailable_thread();
            }
        }
    }

    async fn prepare_removal_detach(&mut self, removed: &HashSet<ThreadId>) -> detach::DetachPlan {
        let mut plan = self
            .prepare_detach_plan(DetachScope::Navigation { destination: None })
            .await;
        plan.targets.retain(|target| {
            removed.contains(&target.thread_id)
                && (matches!(target.kind, detach::DetachKind::Side { .. })
                    || self
                        .thread_event_channels
                        .get(&target.thread_id)
                        .is_some_and(|channel| channel.attachment() == ThreadEventAttachment::Live))
        });
        plan
    }

    #[expect(
        clippy::expect_used,
        reason = "The matching removal completion exclusively owns the connection lane."
    )]
    pub(super) async fn finish_removal_completion(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        completion: ControlCompletion,
    ) -> Result<AppRunControl> {
        let lane = self
            .server_operations
            .lanes
            .remove(&OperationLane::Connection)
            .expect("removal completion owns the connection lane");
        let SubmissionRequest::Operation(operation) = &lane.active.request else {
            unreachable!()
        };
        let ServerOperation::Connection(ConnectionMutation::Removal(operation)) = &**operation
        else {
            unreachable!()
        };
        let operation = operation.clone();
        let control = AppRunControl::Continue;
        let outcome = match completion.outcome {
            SubmissionOutcome::Operation(Ok(OperationReceipt::Removal(receipt))) => {
                match (operation, receipt) {
                    (
                        RemovalOperation::Inspect {
                            request,
                            primary: old_primary,
                            voice: old_voice,
                        },
                        RemovalReceipt::Inspected { primary, voice },
                    ) => {
                        // Input remains live while metadata is read. Never stop a replacement
                        // voice owner using a relationship read for its predecessor.
                        if self.voice_owner_thread_id() != old_voice
                            || self.primary_thread_id != old_primary
                        {
                            let next = RemovalOperation::Inspect {
                                request,
                                primary: self.primary_thread_id,
                                voice: self.voice_owner_thread_id(),
                            };
                            self.continue_removal(server, lane, next);
                            return Ok(control);
                        }
                        let target = RemovalTarget { request, primary };
                        let removed = self.removal_threads(&target);
                        let mut plan = self
                            .prepare_detach_plan(DetachScope::Navigation { destination: None })
                            .await;
                        plan.targets.retain(|entry| {
                            (matches!(target.request.source, RemovalSource::Current)
                                && !matches!(self.app_server_target, AppServerTarget::Embedded)
                                || matches!(target.request.source, RemovalSource::Overview)
                                    && target.primary.is_some())
                                && matches!(entry.kind, detach::DetachKind::Side { .. })
                                && removed.contains(&entry.thread_id)
                        });
                        if let Some(voice) = voice {
                            plan.voice = self
                                .voice_widget_for_thread(voice)
                                .and_then(ChatWidget::reset_realtime_conversation);
                            self.retire_background_voice();
                        }
                        let next = if plan.is_empty() {
                            RemovalOperation::Mutate(target)
                        } else {
                            RemovalOperation::Before { target, plan }
                        };
                        self.continue_removal(server, lane, next);
                        return Ok(control);
                    }
                    (
                        RemovalOperation::Before { target, plan },
                        RemovalReceipt::Detached(receipt),
                    ) => {
                        let certainty = detach_certainty(&receipt);
                        self.finish_detach_plan(&plan, receipt).await;
                        match certainty {
                            DetachCertainty::Confirmed => {
                                self.continue_removal(
                                    server,
                                    lane,
                                    RemovalOperation::Mutate(target),
                                );
                                return Ok(control);
                            }
                            DetachCertainty::Uncertain => {
                                self.hold_uncertain_removal(lane);
                                return Ok(control);
                            }
                            DetachCertainty::Rejected => self.present_removal_failure(
                                &target,
                                &"A frontend attachment could not be closed. The task was not removed; retry the action when ready.",
                            ),
                        }
                    }
                    (RemovalOperation::Mutate(target), RemovalReceipt::Refused(error)) => {
                        self.invalidate_removal_overview_refresh();
                        self.present_removal_failure(&target, &error);
                        self.refresh_agents_overview_threads(server);
                    }
                    (RemovalOperation::Mutate(target), RemovalReceipt::Mutated(result)) => {
                        self.invalidate_removal_overview_refresh();
                        if let Err(failure) = result {
                            if target.primary.is_some() {
                                self.continue_removal(
                                    server,
                                    lane,
                                    RemovalOperation::Reconcile { target, failure },
                                );
                                return Ok(control);
                            }
                            self.present_removal_failure(&target, &failure);
                            self.refresh_agents_overview_threads(server);
                        } else {
                            let removed = self.removal_threads(&target);
                            self.project_removed_task(&target, &removed).await;
                            if self.removal_view_matches(&target) {
                                let plan = self.prepare_removal_detach(&removed).await;
                                self.continue_removal(
                                    server,
                                    lane,
                                    RemovalOperation::After {
                                        target,
                                        failure: None,
                                        plan,
                                    },
                                );
                                return Ok(control);
                            }
                            self.repaint_agents_overview();
                            self.refresh_agents_overview_threads(server);
                        }
                    }
                    (
                        RemovalOperation::Reconcile { target, failure },
                        RemovalReceipt::PrimaryLoaded(loaded),
                    ) => {
                        if !loaded {
                            self.pause_removed_primary(&target);
                        }
                        if !loaded && self.removal_view_matches(&target) {
                            let removed = self.removal_threads(&target);
                            let plan = self.prepare_removal_detach(&removed).await;
                            self.continue_removal(
                                server,
                                lane,
                                RemovalOperation::After {
                                    target,
                                    failure: Some(failure),
                                    plan,
                                },
                            );
                            return Ok(control);
                        }
                        self.present_removal_failure(&target, &failure);
                        self.refresh_agents_overview_threads(server);
                    }
                    (
                        RemovalOperation::After {
                            target,
                            failure,
                            plan,
                        },
                        RemovalReceipt::Detached(receipt),
                    ) => {
                        let certainty = detach_certainty(&receipt);
                        self.finish_detach_plan(&plan, receipt).await;
                        if matches!(certainty, DetachCertainty::Uncertain) {
                            self.hold_uncertain_removal(lane);
                            return Ok(control);
                        }
                        if matches!(certainty, DetachCertainty::Rejected) {
                            self.chat_widget.add_error_message("Task removal finished, but some frontend attachments could not be closed. No detach was retried.".into());
                        }
                        if matches!(certainty, DetachCertainty::Confirmed)
                            && self.removal_view_matches(&target)
                        {
                            if let Err(error) = self.reset_for_thread_switch(tui) {
                                self.server_operations
                                    .lanes
                                    .insert(OperationLane::Connection, lane);
                                return Err(error);
                            }
                            let input = self.chat_widget.capture_thread_input_state().and_then(
                                |mut input| {
                                    input.questions = None;
                                    input.has_unsent_input().then_some(input)
                                },
                            );
                            self.pending_thread_switch_resets += 1;
                            self.app_event_tx
                                .send(AppEvent::ResetTranscriptForThreadSwitch);
                            self.reset_thread_event_state().await;
                            let init = self.chatwidget_init_for_forked_or_resumed_thread(
                                tui,
                                self.config.clone(),
                                None,
                            );
                            self.replace_chat_widget(ChatWidget::new_with_app_event(init));
                            if let Some(input) = input {
                                self.chat_widget.restore_thread_input_state(
                                    Some(input),
                                    crate::chatwidget::ThreadInputStateRestoreMode {
                                        preserve_in_flight_turn: false,
                                    },
                                );
                                self.chat_widget.restore_session_draft();
                                self.chat_widget.add_info_message(
                                    "Unsent input was kept as a new draft.".into(),
                                    None,
                                );
                            }
                            self.open_agents_overview(server);
                        }
                        if let Some(failure) = failure {
                            self.present_removal_failure(&target, &failure);
                        }
                    }
                    _ => unreachable!("removal stage and receipt disagree"),
                }
                SubmissionOutcome::Operation(Ok(OperationReceipt::Removal(
                    RemovalReceipt::Finished,
                )))
            }
            outcome => {
                if let RemovalOperation::Reconcile { target, failure } = operation {
                    self.pause_removed_primary(&target);
                    self.present_removal_failure(&target, &failure);
                }
                outcome
            }
        };
        self.server_operations
            .lanes
            .insert(OperationLane::Connection, lane);
        self.finish_submission(
            server,
            completion.scope,
            completion.id,
            completion.epoch,
            outcome,
        )
        .await?;
        Ok(control)
    }

    #[cfg(test)]
    pub(in crate::app) async fn settle_removal(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
    ) -> Result<AppRunControl> {
        Box::pin(tokio::time::timeout(Duration::from_secs(5), async {
            while self.server_operations.lanes.get(&OperationLane::Connection).is_some_and(|lane| {
                !matches!(lane.state, LaneState::Uncertain)
                    && std::iter::once(&lane.active).chain(lane.waiting.iter()).any(|pending| {
                        matches!(&pending.request, SubmissionRequest::Operation(operation)
                            if matches!(&**operation, ServerOperation::Connection(ConnectionMutation::Removal(_))))
                    })
            }) {
                tokio::select! {
                    completion = self.thread_delivery.completion_rx.recv() => {
                        self.handle_local_thread_completion(server, completion.expect("local receipt channel open")).await?;
                    }
                    completion = self.server_operations.control_rx.recv() => {
                        let control = Box::pin(self.handle_server_completion(tui, server, completion.expect("server receipt channel open"))).await?;
                        if !matches!(control, AppRunControl::Continue) { return Ok(control); }
                    }
                }
            }
            Ok(AppRunControl::Continue)
        })).await?
    }

    fn hold_uncertain_removal(&mut self, mut lane: SubmissionLane) {
        lane.state = LaneState::Uncertain;
        self.server_operations
            .lanes
            .insert(OperationLane::Connection, lane);
        self.chat_widget.add_error_message("Task detachment was not fully confirmed. No removal or detach was retried. Close and reopen this TUI to resynchronize.".into());
        self.refresh_submission_preview();
    }

    fn invalidate_removal_overview_refresh(&mut self) {
        if let Some(refresh) = self.agents_overview.refresh_task.take() {
            refresh.abort();
        }
        self.agents_overview.request_id = None;
        self.agents_overview.refresh_show_more = false;
        self.agents_overview.active_refresh_thread_ids.clear();
        self.agents_overview
            .view_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .loading = false;
        self.agents_overview.refresh_pending = false;
        self.agents_overview.refresh_notifications.clear();
    }

    fn removal_threads(&self, target: &RemovalTarget) -> HashSet<ThreadId> {
        let mut removed = HashSet::from([target.request.thread_id]);
        removed.extend(target.primary);
        loop {
            let count = removed.len();
            for (id, thread) in &self.agents_overview.threads {
                let Some(thread) = thread else { continue };
                let parent = thread
                    .parent_thread_id
                    .as_deref()
                    .and_then(|id| ThreadId::from_string(id).ok())
                    .or(match &thread.source {
                        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                            parent_thread_id,
                            ..
                        }) => Some(*parent_thread_id),
                        _ => None,
                    });
                if thread.session_id == target.request.thread_id.to_string()
                    || parent.is_some_and(|id| removed.contains(&id))
                {
                    removed.insert(*id);
                }
            }
            for (id, side) in &self.side_threads {
                if removed.contains(&side.parent_thread_id) {
                    removed.insert(*id);
                }
            }
            if removed.len() == count {
                break;
            }
        }
        removed
    }

    async fn project_removed_task(&mut self, target: &RemovalTarget, removed: &HashSet<ThreadId>) {
        self.prepare_agents_overview_removal(removed);
        self.pause_removed_primary(target);
        for &id in removed {
            if let Some(channel) = self.thread_event_channels.get_mut(&id) {
                channel.mark_replay_only();
                let store = Arc::clone(&channel.store);
                let drafts = self.take_unsent_thread_operations(id, &store).await;
                self.restore_submission_drafts(id, &store, drafts, DraftDisposition::Edit)
                    .await;
            }
            if self.chat_widget.thread_id() == Some(id) {
                self.chat_widget.pause_unavailable_thread();
            }
            self.remove_agents_overview_thread(id);
            self.agents_overview.activity.remove(&id);
            self.agents_overview.last_messages.remove(&id);
            self.agents_overview.usage.remove(&id);
            self.agents_overview.refresh_thread_ids.remove(&id);
            self.agents_overview.dispatched_requests.remove(&id);
        }
    }
}
