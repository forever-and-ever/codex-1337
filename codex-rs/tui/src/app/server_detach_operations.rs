//! Subscription detachment is not runtime shutdown. A plan retains each local store it owns.

use super::App;
use super::Arc;
use super::ClientRequest;
use super::DraftDisposition;
use super::Duration;
use super::Mutex;
use super::Result;
use super::ThreadEventAttachment;
use super::ThreadEventStore;
use super::ThreadId;
use super::TypedRequestError;
use super::Uuid;
use super::active_turn_interrupt_race;
use crate::app_server_session::typed_requests::SessionRpc;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

pub(in crate::app) enum DetachScope {
    Exit,
    VoiceOnly,
    Navigation { destination: Option<ThreadId> },
    NavigationKeepingRetainedSides { destination: ThreadId },
}

#[derive(Clone)]
pub(in crate::app) struct DetachPlan {
    pub(super) targets: Vec<DetachTarget>,
    pub(super) voice: Option<ThreadId>,
}

impl DetachPlan {
    pub(in crate::app) fn origin(&self) -> Option<ThreadId> {
        self.voice
            .or_else(|| self.targets.first().map(|target| target.thread_id))
    }

    pub(in crate::app) fn is_empty(&self) -> bool {
        self.voice.is_none() && self.targets.is_empty()
    }
}

#[derive(Clone)]
pub(super) struct DetachTarget {
    pub(super) thread_id: ThreadId,
    pub(super) store: Arc<Mutex<ThreadEventStore>>,
    pub(super) kind: DetachKind,
}

#[derive(Clone)]
pub(super) enum DetachKind {
    Conversation,
    FailedSide,
    Side { turn_id: String, replay_only: bool },
}

#[derive(Debug)]
pub(in crate::app) enum DetachReceipt {
    Complete(Vec<DetachedTarget>),
    BudgetExpired,
}

#[derive(Debug)]
pub(in crate::app) struct DetachedTarget {
    pub(super) thread_id: ThreadId,
    pub(super) result: Result<(), TypedRequestError>,
}

pub(in crate::app) async fn unsubscribe(
    handle: &AppServerRequestHandle,
    id: Uuid,
    thread_id: ThreadId,
) -> Result<(), TypedRequestError> {
    handle
        .request_typed::<api::ThreadUnsubscribeResponse>(ClientRequest::ThreadUnsubscribe {
            request_id: api::RequestId::String(format!(
                "tui-operation-{id}-{thread_id}-unsubscribe"
            )),
            params: api::ThreadUnsubscribeParams {
                thread_id: thread_id.to_string(),
            },
        })
        .await
        .map(|_| ())
}

pub(super) async fn send(
    handle: &AppServerRequestHandle,
    id: Uuid,
    plan: &DetachPlan,
) -> Vec<DetachedTarget> {
    if let Some(thread_id) = plan.voice {
        // Audio helper resources have already been retired locally. Stop is best effort, bounded.
        let stop = handle.request_typed::<api::ThreadRealtimeStopResponse>(
            ClientRequest::ThreadRealtimeStop {
                request_id: api::RequestId::String(format!("tui-operation-{id}-voice-stop")),
                params: api::ThreadRealtimeStopParams {
                    thread_id: thread_id.to_string(),
                },
            },
        );
        if !matches!(
            tokio::time::timeout(Duration::from_millis(200), stop).await,
            Ok(Ok(_))
        ) {
            tracing::warn!(%thread_id, "voice stop was not confirmed before detach");
        }
    }
    let mut results = Vec::new();
    for target in &plan.targets {
        let thread_id = target.thread_id;
        let result = async {
            if let DetachKind::Side { turn_id, replay_only } = &target.kind {
                let mut turn_id = turn_id.clone();
                for attempt in 0..2 {
                    let interrupt = handle.turn_interrupt(api::RequestId::String(format!("tui-operation-{id}-{thread_id}-side-interrupt-{attempt}")), api::TurnInterruptParams { thread_id: thread_id.to_string(), turn_id: turn_id.clone() }).await;
                    match interrupt {
                        Ok(_) => break,
                        Err(error) => {
                            let gone = *replay_only && matches!(&error, TypedRequestError::Server { method, source }
                                if method == "turn/interrupt" && source.code == -32600 && source.message == format!("thread not found: {thread_id}"));
                            if gone { break; }
                            if attempt == 0 && let Some(actual) = active_turn_interrupt_race(&error) && actual != turn_id {
                                turn_id = actual;
                                continue;
                            }
                            return Err(error);
                        }
                    }
                }
            }
            if matches!(target.kind, DetachKind::FailedSide) {
                let _: api::ThreadDeleteResponse = handle.request_typed(ClientRequest::ThreadDelete {
                    request_id: api::RequestId::String(format!("tui-operation-{id}-{thread_id}-rollback")),
                    params: api::ThreadDeleteParams { thread_id: thread_id.to_string() },
                }).await?;
                Ok(())
            } else { unsubscribe(handle, id, thread_id).await }
        }.await;
        results.push(DetachedTarget { thread_id, result });
    }
    results
}

pub(in crate::app) async fn close_side_subscription(
    handle: &AppServerRequestHandle,
    id: Uuid,
    thread_id: ThreadId,
    store: Arc<Mutex<ThreadEventStore>>,
    turn_id: String,
    replay_only: bool,
) -> Result<(), TypedRequestError> {
    let plan = DetachPlan {
        targets: vec![DetachTarget {
            thread_id,
            store,
            kind: DetachKind::Side {
                turn_id,
                replay_only,
            },
        }],
        voice: None,
    };
    send(handle, id, &plan)
        .await
        .into_iter()
        .try_for_each(|target| target.result)
}

impl App {
    #[expect(
        clippy::needless_collect,
        reason = "Snapshot side identifiers before mutably borrowing App to prepare their detachment."
    )]
    pub(in crate::app) async fn prepare_detach_plan(&mut self, scope: DetachScope) -> DetachPlan {
        let destination = match scope {
            DetachScope::Navigation { destination } => destination,
            DetachScope::NavigationKeepingRetainedSides { destination } => Some(destination),
            _ => None,
        };
        let voice = if matches!(
            scope,
            DetachScope::Navigation { .. } | DetachScope::NavigationKeepingRetainedSides { .. }
        ) {
            None
        } else {
            let thread_id = self
                .background_voice
                .as_mut()
                .and_then(|owner| owner.reset_realtime_conversation())
                .or_else(|| self.chat_widget.reset_realtime_conversation());
            self.retire_background_voice();
            thread_id
        };
        match scope {
            DetachScope::Exit
            | DetachScope::Navigation { .. }
            | DetachScope::NavigationKeepingRetainedSides { .. } => {}
            DetachScope::VoiceOnly => {
                return DetachPlan {
                    targets: Vec::new(),
                    voice,
                };
            }
        }
        let mut targets = Vec::new();
        // Side conversations stop before unsubscribing; ordinary conversations keep running.
        for thread_id in self.side_threads.keys().copied().collect::<Vec<_>>() {
            if matches!(scope, DetachScope::NavigationKeepingRetainedSides { .. })
                || destination == Some(thread_id)
            {
                continue;
            }
            let channel = self.ensure_thread_channel(thread_id);
            let store = Arc::clone(&channel.store);
            let replay_only = channel.attachment() == ThreadEventAttachment::ReplayOnly;
            let turn_id = self
                .active_turn_id_for_thread(thread_id)
                .await
                .unwrap_or_default();
            targets.push(DetachTarget {
                thread_id,
                store,
                kind: DetachKind::Side {
                    turn_id,
                    replay_only,
                },
            });
        }
        for (&thread_id, channel) in &self.thread_event_channels {
            if destination == Some(thread_id)
                || self.side_threads.contains_key(&thread_id)
                || self.voice_owner_thread_id() == Some(thread_id)
                || self
                    .agents_overview
                    .dispatched_requests
                    .contains_key(&thread_id)
                || self.agents_overview.blank_sessions.contains_key(&thread_id)
            {
                continue;
            }
            targets.push(DetachTarget {
                thread_id,
                store: Arc::clone(&channel.store),
                kind: DetachKind::Conversation,
            });
        }
        DetachPlan { targets, voice }
    }

    #[expect(
        clippy::expect_used,
        reason = "The target channel was checked against the detach receipt before mutation."
    )]
    pub(in crate::app) async fn finish_detach_plan(
        &mut self,
        plan: &DetachPlan,
        receipt: DetachReceipt,
    ) {
        let DetachReceipt::Complete(results) = receipt else {
            tracing::warn!(
                "detach escape budget expired; subscription outcomes remain unknown and were not retried"
            );
            return;
        };
        for result in results {
            let Some(target) = plan
                .targets
                .iter()
                .find(|target| target.thread_id == result.thread_id)
            else {
                continue;
            };
            if !self
                .thread_event_channels
                .get(&target.thread_id)
                .is_some_and(|channel| Arc::ptr_eq(&channel.store, &target.store))
            {
                continue;
            }
            match (&target.kind, result.result) {
                (DetachKind::Side { .. } | DetachKind::FailedSide, Ok(())) => {
                    self.abandoned_side_threads.insert(target.thread_id);
                    let drafts = self
                        .take_unsent_thread_operations(target.thread_id, &target.store)
                        .await;
                    self.pause_detached_thread_input(target).await;
                    self.discard_thread_local_state(target.thread_id).await;
                    self.restore_submission_drafts(
                        target.thread_id,
                        &target.store,
                        drafts,
                        DraftDisposition::Edit,
                    )
                    .await;
                }
                (DetachKind::Side { .. } | DetachKind::FailedSide, Err(error)) => {
                    self.add_agents_overview_error(format!(
                        "Closing side conversation {} was not confirmed: {error}",
                        target.thread_id
                    ));
                }
                (DetachKind::Conversation, Ok(())) => {
                    let thread_id = target.thread_id;
                    let drafts = self
                        .take_unsent_thread_operations(thread_id, &target.store)
                        .await;
                    self.thread_event_channels
                        .get_mut(&thread_id)
                        .expect("matching detach target checked")
                        .mark_replay_only();
                    self.abort_thread_event_listener(thread_id);
                    self.pending_server_profiles.remove(&thread_id);
                    self.pause_detached_thread_input(target).await;
                    self.restore_submission_drafts(
                        thread_id,
                        &target.store,
                        drafts,
                        DraftDisposition::Edit,
                    )
                    .await;
                }
                (DetachKind::Conversation, Err(error)) => {
                    tracing::warn!(thread_id = %target.thread_id, %error, "unsubscribe not confirmed; local attachment retained");
                }
            }
        }
    }

    async fn pause_detached_thread_input(&mut self, target: &DetachTarget) {
        let thread_id = target.thread_id;
        if self.chat_widget.thread_id() == Some(thread_id) {
            self.chat_widget.pause_unavailable_thread();
        }
        if let Some(input) = self.agents_overview.input_states.get_mut(&thread_id) {
            input.pause_unavailable_thread();
        }
        if let Some(input) = target.store.lock().await.input_state.as_mut() {
            input.pause_unavailable_thread();
        }
    }
}
