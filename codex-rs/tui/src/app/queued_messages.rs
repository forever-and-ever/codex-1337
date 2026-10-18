//! Project the durable queue into the existing pending-input controls.

use super::App;
use crate::app_command::PendingSteerCancelAction;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::chatwidget::UserMessage;
use crate::chatwidget::queued_message_text;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::QueuedSubmission;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadQueueDeleteParams;
use codex_app_server_protocol::ThreadQueueDeleteResponse;
use codex_app_server_protocol::ThreadQueueListParams;
use codex_app_server_protocol::ThreadQueueListResponse;
use codex_protocol::ThreadId;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;
use uuid::Uuid;

#[derive(Default)]
pub(super) struct QueuedMessages {
    reads: HashMap<ThreadId, Uuid>,
    read_again: HashSet<ThreadId>,
    pub(super) mutations: HashMap<ThreadId, QueueMutation>,
}

pub(super) struct QueueMutation {
    pub(super) item: QueuedSubmission,
    pub(super) action: PendingSteerCancelAction,
}

impl QueuedMessages {
    /// Drops every operation started on the current connection and returns the edits whose
    /// outcome is unknown, so they can return to their drafts; nothing is resubmitted.
    pub(super) fn retire(&mut self) -> Vec<(ThreadId, QueuedSubmission)> {
        self.reads.clear();
        self.read_again.clear();
        self.mutations
            .drain()
            .filter_map(|(thread_id, mutation)| {
                (mutation.action == PendingSteerCancelAction::Edit)
                    .then_some((thread_id, mutation.item))
            })
            .collect()
    }
}

#[derive(Debug)]
pub(crate) enum QueueEvent {
    Refresh(ThreadId),
    Loaded {
        request: Uuid,
        thread_id: ThreadId,
        result: Result<Vec<QueuedSubmission>, String>,
    },
    Remove {
        thread_id: ThreadId,
        item: QueuedSubmission,
        action: PendingSteerCancelAction,
    },
    Removed {
        thread_id: ThreadId,
        result: Result<bool, String>,
    },
}

impl App {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "MAX_QUEUE_ITEMS is the server-defined 100-item cap, below u32::MAX."
    )]
    pub(super) async fn handle_queue_event(
        &mut self,
        server: &AppServerSession,
        event: QueueEvent,
    ) {
        match event {
            QueueEvent::Refresh(thread_id) => {
                if self.queued_messages.reads.contains_key(&thread_id) {
                    let _ = self.queued_messages.read_again.insert(thread_id);
                    return;
                }
                let request = Uuid::new_v4();
                let _ = self.queued_messages.reads.insert(thread_id, request);
                let handle = server.request_handle();
                let events = self.app_event_tx.clone();
                drop(tokio::spawn(async move {
                    let result = tokio::time::timeout(
                        Duration::from_secs(/*secs*/ 15),
                        handle.request_typed::<ThreadQueueListResponse>(
                            ClientRequest::ThreadQueueList {
                                request_id: RequestId::String(format!("queue-list-{request}")),
                                params: ThreadQueueListParams {
                                    thread_id: thread_id.to_string(),
                                    cursor: None,
                                    limit: Some(codex_state::MAX_QUEUE_ITEMS as u32),
                                },
                            },
                        ),
                    )
                    .await
                    .map_err(|_| "Queue refresh timed out".to_string())
                    .and_then(|result| {
                        result
                            .map(|page| page.data)
                            .map_err(|error| error.to_string())
                    });
                    events.send(AppEvent::Queue(QueueEvent::Loaded {
                        request,
                        thread_id,
                        result,
                    }));
                }));
            }
            QueueEvent::Loaded {
                request,
                thread_id,
                result,
            } => {
                if self.queued_messages.reads.get(&thread_id) != Some(&request) {
                    return;
                }
                let _ = self.queued_messages.reads.remove(&thread_id);
                if self.chat_widget.thread_id() == Some(thread_id) {
                    match result {
                        Ok(items) => self.chat_widget.update_server_queue(
                            items,
                            self.queued_messages.mutations.contains_key(&thread_id),
                        ),
                        Err(error) => {
                            tracing::warn!(%thread_id, %error, "could not refresh queued messages");
                        }
                    }
                }
                if self.queued_messages.read_again.remove(&thread_id) {
                    self.app_event_tx
                        .send(AppEvent::Queue(QueueEvent::Refresh(thread_id)));
                }
            }
            QueueEvent::Remove {
                thread_id,
                item,
                action,
            } => {
                if self.queued_messages.mutations.contains_key(&thread_id) {
                    return;
                }
                let queued_submission_id = item.id.clone();
                let _ = self
                    .queued_messages
                    .mutations
                    .insert(thread_id, QueueMutation { item, action });
                if self.chat_widget.thread_id() == Some(thread_id) {
                    self.chat_widget.project_server_queue_mutation(true);
                }
                let handle = server.request_handle();
                let events = self.app_event_tx.clone();
                drop(tokio::spawn(async move {
                    let result = tokio::time::timeout(
                        Duration::from_secs(/*secs*/ 15),
                        handle.request_typed::<ThreadQueueDeleteResponse>(
                            ClientRequest::ThreadQueueDelete {
                                request_id: RequestId::String(format!(
                                    "queue-remove-{}",
                                    Uuid::new_v4()
                                )),
                                params: ThreadQueueDeleteParams {
                                    thread_id: thread_id.to_string(),
                                    queued_submission_id,
                                },
                            },
                        ),
                    )
                    .await
                    .map_err(|_| "Queue cancellation timed out".to_string())
                    .and_then(|result| {
                        result
                            .map(|response| response.deleted)
                            .map_err(|error| error.to_string())
                    });
                    events.send(AppEvent::Queue(QueueEvent::Removed { thread_id, result }));
                }));
            }
            QueueEvent::Removed { thread_id, result } => {
                let Some(QueueMutation { item, action }) =
                    self.queued_messages.mutations.remove(&thread_id)
                else {
                    return;
                };
                // Ignore any pre-cancellation snapshot still in flight.
                let _ = self.queued_messages.reads.remove(&thread_id);
                let _ = self.queued_messages.read_again.remove(&thread_id);
                // An uncertain acknowledgement must not lose an explicitly requested draft.
                // Never submit it automatically: the original may already have been sent.
                if self.chat_widget.thread_id() == Some(thread_id) {
                    self.chat_widget
                        .finish_server_queue_removal(&item.id, &result);
                }
                if action == PendingSteerCancelAction::Edit && result != Ok(false) {
                    self.restore_queued_edit(thread_id, item).await;
                }
                if let Err(error) = result {
                    self.chat_widget.add_error_message(format!("Could not confirm queue cancellation for {thread_id}: {error}. Any requested edit was recovered as a draft; check before resending."));
                }
                self.app_event_tx
                    .send(AppEvent::Queue(QueueEvent::Refresh(thread_id)));
            }
        }
    }

    pub(super) async fn restore_queued_edit(
        &mut self,
        thread_id: ThreadId,
        item: QueuedSubmission,
    ) {
        let Some(text) = queued_message_text(&item) else {
            return;
        };
        if self.chat_widget.thread_id() == Some(thread_id) {
            self.chat_widget
                .restore_user_message_to_composer(UserMessage::from(text));
        } else if let Some(channel) = self.thread_event_channels.get(&thread_id)
            && let Some(input) = channel.store.lock().await.input_state.as_mut()
        {
            input.restore_queued_text(text);
        } else {
            self.chat_widget
                .restore_user_message_to_composer(UserMessage::from(text));
            self.chat_widget.add_warning_message(format!("The original thread {thread_id} is no longer open. Its queued edit was recovered into this draft."));
        }
    }
}
