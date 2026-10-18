//! Loop-owned admission for thread events. Workers report their one operation result through a
//! bounded ingress; they never reserve a slot in the visible thread's receiver themselves.

use super::*;
use codex_app_server_protocol::RequestId as AppServerRequestId;
use codex_app_server_protocol::UserVerificationProof;

const LOCAL_COMPLETION_CAPACITY: usize = 64;

#[derive(Debug)]
pub(super) enum LocalThreadCompletion {
    Submission {
        thread_id: ThreadId,
        id: Uuid,
        epoch: u64,
        outcome: Box<turn_submissions::SubmissionOutcome>,
    },
    HistoryLookup {
        thread_id: ThreadId,
        event: HistoryLookupResponse,
    },
    FeedbackSubmitted {
        origin_thread_id: Option<ThreadId>,
        category: FeedbackCategory,
        include_logs: bool,
        result: Result<String, String>,
    },
    VerificationFinished {
        thread_id: ThreadId,
        server_name: String,
        request_id: AppServerRequestId,
        attempt_id: Uuid,
        result: Result<UserVerificationProof, String>,
    },
}

struct PendingThreadDelivery {
    thread_id: ThreadId,
    store: Arc<Mutex<ThreadEventStore>>,
    event: ThreadBufferedEvent,
}

pub(super) struct ThreadDelivery {
    pub(super) completion_tx: mpsc::Sender<LocalThreadCompletion>,
    pub(super) completion_rx: mpsc::Receiver<LocalThreadCompletion>,
    pending: Option<PendingThreadDelivery>,
    pub(super) primary_replay_thread: Option<ThreadId>,
}

impl ThreadDelivery {
    pub(super) fn new() -> Self {
        let (completion_tx, completion_rx) = mpsc::channel(LOCAL_COMPLETION_CAPACITY);
        Self {
            completion_tx,
            completion_rx,
            pending: None,
            primary_replay_thread: None,
        }
    }

    pub(super) fn is_blocked(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn has_pending_work(&self) -> bool {
        self.is_blocked() || self.primary_replay_thread.is_some() || !self.completion_rx.is_empty()
    }
}

impl App {
    pub(super) async fn admit_thread_event(
        &mut self,
        thread_id: ThreadId,
        event: ThreadBufferedEvent,
    ) {
        // Backend and completion ingress stop polling while occupied. Generic AppEvents apply
        // widget/history effects, but no longer carry thread-producing worker results.
        assert!(
            self.thread_delivery.pending.is_none(),
            "thread admission overtook a pending event"
        );
        let channel = self.ensure_thread_channel(thread_id);
        match channel.sender.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(event)) => {
                let store = Arc::clone(&channel.store);
                self.thread_delivery.pending = Some(PendingThreadDelivery {
                    thread_id,
                    store,
                    event,
                });
            }
            Err(TrySendError::Closed(event)) => {
                tracing::warn!(%thread_id, "thread event channel closed");
                if matches!(
                    &event,
                    ThreadBufferedEvent::HistoryEntryResponse(HistoryLookupResponse::Batch { .. })
                ) {
                    channel.store.lock().await.push_buffered_event(event);
                }
            }
        }
    }

    pub(super) async fn retry_pending_thread_delivery(&mut self) {
        let Some(pending) = self.thread_delivery.pending.take() else {
            return;
        };
        let Some(channel) = self.thread_event_channels.get_mut(&pending.thread_id) else {
            tracing::warn!(thread_id = %pending.thread_id, "pending delivery's thread was retired");
            return;
        };
        {
            let mut store = channel.store.lock().await;
            if !store.active || !Arc::ptr_eq(&pending.store, &channel.store) {
                // Replacement invalidates old approvals just as dropping their original receiver did;
                // never deliver an old request into the new channel's freshly hydrated request ledger.
                // Other events already took the normal store/replay policy before try_send. Active
                // history batches avoid a deep copy, so preserve that batch on detach or reconnect.
                if matches!(
                    &pending.event,
                    ThreadBufferedEvent::HistoryEntryResponse(HistoryLookupResponse::Batch { .. })
                ) {
                    store.push_buffered_event(pending.event);
                }
                return;
            }
        }
        match channel.sender.try_send(pending.event) {
            Ok(()) => {}
            Err(TrySendError::Full(event)) => {
                self.thread_delivery.pending = Some(PendingThreadDelivery {
                    thread_id: pending.thread_id,
                    store: pending.store,
                    event,
                });
            }
            Err(TrySendError::Closed(event)) => {
                tracing::warn!(thread_id = %pending.thread_id, "pending thread event channel closed");
                if matches!(
                    &event,
                    ThreadBufferedEvent::HistoryEntryResponse(HistoryLookupResponse::Batch { .. })
                ) {
                    channel.store.lock().await.push_buffered_event(event);
                }
            }
        }
    }

    pub(super) async fn handle_local_thread_completion(
        &mut self,
        app_server: &mut AppServerSession,
        completion: LocalThreadCompletion,
    ) -> Result<()> {
        match completion {
            LocalThreadCompletion::Submission {
                thread_id,
                id,
                epoch,
                outcome,
            } => {
                self.finish_submission(app_server, thread_id, id, epoch, *outcome)
                    .await?;
            }
            LocalThreadCompletion::HistoryLookup { thread_id, event } => {
                self.enqueue_thread_history_entry_response(thread_id, event)
                    .await?;
            }
            LocalThreadCompletion::FeedbackSubmitted {
                origin_thread_id,
                category,
                include_logs,
                result,
            } => {
                self.handle_feedback_submitted(origin_thread_id, category, include_logs, result)
                    .await;
            }
            LocalThreadCompletion::VerificationFinished {
                thread_id,
                server_name,
                request_id,
                attempt_id,
                result,
            } => {
                Box::pin(self.finish_user_verification(
                    app_server,
                    thread_id,
                    server_name,
                    request_id,
                    attempt_id,
                    result,
                ))
                .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn admit_next_primary_event(&mut self) -> Result<()> {
        let Some(thread_id) = self
            .thread_delivery
            .primary_replay_thread
            .or(self.primary_thread_id)
        else {
            return Ok(());
        };
        let Some(event) = self.pending_primary_events.pop_front() else {
            return Ok(());
        };
        match event {
            ThreadBufferedEvent::Notification(notification) => {
                self.enqueue_thread_notification(thread_id, *notification)
                    .await?;
            }
            ThreadBufferedEvent::Request(request) => {
                self.enqueue_thread_request(thread_id, *request).await?;
            }
            ThreadBufferedEvent::HistoryEntryResponse(event) => {
                self.enqueue_thread_history_entry_response(thread_id, event)
                    .await?;
            }
            ThreadBufferedEvent::FeedbackSubmission(event) => {
                self.enqueue_thread_feedback_event(thread_id, event).await;
            }
            ThreadBufferedEvent::FarmReceipt {
                thread_id,
                request_id,
            } => {
                self.app_event_tx.send(AppEvent::RestoreFarmReceipt {
                    thread_id,
                    request_id,
                });
            }
        }
        Ok(())
    }

    pub(super) fn finish_primary_event_replay(&mut self) {
        self.thread_delivery.primary_replay_thread = None;
        self.chat_widget
            .set_initial_user_message_submit_suppressed(/*suppressed*/ false);
        self.chat_widget.submit_initial_user_message_if_pending();
        self.queue_farm_receipt_replay();
    }
}
