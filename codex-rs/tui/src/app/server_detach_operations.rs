//! Subscription detachment is not runtime shutdown. A plan retains each local store it owns.

use super::*;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

pub(in crate::app) enum DetachScope {
    Exit,
    VoiceOnly,
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
    pub(super) result: Result<(), String>,
}

pub(super) async fn unsubscribe(
    handle: &AppServerRequestHandle,
    id: Uuid,
    thread_id: ThreadId,
) -> Result<(), TypedRequestError> {
    handle
        .request_typed::<api::ThreadUnsubscribeResponse>(api::ClientRequest::ThreadUnsubscribe {
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
            api::ClientRequest::ThreadRealtimeStop {
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
                    let interrupt = handle.request_typed::<api::TurnInterruptResponse>(api::ClientRequest::TurnInterrupt {
                        request_id: api::RequestId::String(format!("tui-operation-{id}-{thread_id}-side-interrupt-{attempt}")),
                        params: api::TurnInterruptParams { thread_id: thread_id.to_string(), turn_id: turn_id.clone() },
                    }).await;
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
                            return Err(error.to_string());
                        }
                    }
                }
            }
            unsubscribe(handle, id, thread_id).await.map_err(|error| error.to_string())
        }.await;
        results.push(DetachedTarget { thread_id, result });
    }
    results
}

impl App {
    pub(in crate::app) async fn prepare_detach_plan(&mut self, scope: DetachScope) -> DetachPlan {
        let voice = {
            let thread_id = self
                .background_voice
                .as_mut()
                .and_then(|owner| owner.reset_realtime_conversation())
                .or_else(|| self.chat_widget.reset_realtime_conversation());
            self.retire_background_voice();
            thread_id
        };
        match scope {
            DetachScope::Exit => {}
            DetachScope::VoiceOnly => {
                return DetachPlan {
                    targets: Vec::new(),
                    voice,
                };
            }
        };
        let mut targets = Vec::new();
        // Side conversations stop before unsubscribing; ordinary conversations keep running.
        for thread_id in self.side_threads.keys().copied().collect::<Vec<_>>() {
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
            if self.side_threads.contains_key(&thread_id)
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
                (DetachKind::Side { .. }, Ok(())) => {
                    self.abandoned_side_threads.insert(target.thread_id);
                    self.discard_thread_local_state(target.thread_id).await;
                }
                (DetachKind::Side { .. }, Err(error)) => {
                    self.add_agents_overview_error(format!(
                        "Closing side conversation {} was not confirmed: {error}",
                        target.thread_id
                    ));
                }
                (DetachKind::Conversation, result) => {
                    if let Err(error) = result {
                        tracing::warn!(thread_id = %target.thread_id, %error, "failed to unsubscribe conversation");
                    }
                    self.abort_thread_event_listener(target.thread_id);
                    self.pending_server_profiles.remove(&target.thread_id);
                }
            }
        }
    }
}
