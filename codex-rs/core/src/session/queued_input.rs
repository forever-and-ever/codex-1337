//! A queued submission is not consumed by task registration. Its exact input
//! must cross the canonical rollout barrier before queue cleanup can proceed.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Mutex;

use codex_history::CodexHarnessMetadata;
use codex_history::QueuedInputReceipt;
use codex_history::ResponseItemEnvelope;
use codex_history::RolloutItem;
use codex_protocol::error::CodexErr;
use codex_protocol::openai_models::ModelInfo;
use tokio::sync::OnceCell;

use super::Session;
use super::TurnContext;
use super::TurnInput;
use super::UserInputMetadata;

type QueueCleanup = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

enum PendingQueueInput {
    Awaiting(QueueCleanup),
    Recording(QueueCleanup),
    Accepted(QueueCleanup),
}

pub(crate) struct QueuedInputAdmission {
    pub(crate) id: String,
    pending: Mutex<Option<PendingQueueInput>>,
}

impl QueuedInputAdmission {
    pub(crate) fn new<F>(id: String, cleanup: impl FnOnce() -> F + Send + 'static) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self {
            id,
            pending: Mutex::new(Some(PendingQueueInput::Awaiting(Box::new(move || {
                Box::pin(cleanup())
            })))),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Option<PendingQueueInput>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn begin_recording(&self) {
        let mut pending = self.pending();
        if matches!(*pending, Some(PendingQueueInput::Awaiting(_)))
            && let Some(PendingQueueInput::Awaiting(cleanup)) = pending.take()
        {
            *pending = Some(PendingQueueInput::Recording(cleanup));
        }
    }

    fn accept(&self) -> bool {
        let mut pending = self.pending();
        if matches!(*pending, Some(PendingQueueInput::Recording(_)))
            && let Some(PendingQueueInput::Recording(cleanup)) = pending.take()
        {
            *pending = Some(PendingQueueInput::Accepted(cleanup));
            true
        } else {
            false
        }
    }

    fn take_accepted(&self) -> Option<QueueCleanup> {
        let mut pending = self.pending();
        if matches!(*pending, Some(PendingQueueInput::Accepted(_)))
            && let Some(PendingQueueInput::Accepted(cleanup)) = pending.take()
        {
            Some(cleanup)
        } else {
            None
        }
    }
}

pub(super) fn begin_queued_input_recording(turn: &TurnContext) {
    if let Some(admission) = turn.extension_data.get::<QueuedInputAdmission>() {
        admission.begin_recording();
    }
}

pub(super) fn queued_input_awaits_acceptance(turn: &TurnContext) -> bool {
    turn.extension_data
        .get::<QueuedInputAdmission>()
        .is_some_and(|admission| {
            matches!(
                *admission.pending(),
                Some(PendingQueueInput::Awaiting(_) | PendingQueueInput::Recording(_))
            )
        })
}

pub(crate) fn cancel_unrecorded_queued_input(turn: &TurnContext) {
    if let Some(admission) = turn.extension_data.get::<QueuedInputAdmission>() {
        drop(admission.pending().take());
    }
}

pub(crate) fn queued_input_id(input: &TurnInput) -> Option<&str> {
    match input {
        TurnInput::UserInput { metadata, .. } => metadata.queued_input_id.as_deref(),
        TurnInput::FunctionCallOutput(envelope) => envelope
            .metadata
            .as_ref()?
            .queued_input
            .as_ref()
            .map(|receipt| receipt.id.as_str()),
        _ => None,
    }
}

pub(super) fn receipts_in(items: &[ResponseItemEnvelope]) -> Vec<QueuedInputReceipt> {
    items
        .iter()
        .filter_map(|item| item.metadata.as_ref()?.queued_input.clone())
        .collect()
}

pub(super) fn prompt_envelope_metadata(
    input: &UserInputMetadata,
    turn_id: &str,
) -> Option<CodexHarnessMetadata> {
    (input.acceptance_order.is_some() || input.queued_input_id.is_some()).then(|| {
        CodexHarnessMetadata {
            user_input_order: input.acceptance_order,
            queued_input: input.queued_input_id.clone().map(|id| QueuedInputReceipt {
                id,
                turn_id: turn_id.to_owned(),
            }),
            ..Default::default()
        }
    })
}

#[derive(Default)]
struct QueuedInputReceipts(OnceCell<Mutex<HashMap<String, String>>>);

impl Session {
    pub(crate) async fn queued_input_turn_id(&self, id: &str) -> Result<Option<String>, CodexErr> {
        self.conversation.check_history_recovery()?;
        let live = self
            .live_thread_for_persistence("accept queued input")
            .map_err(io::Error::other)?;
        let runtime = self
            .services
            .thread_extension_data
            .get_or_init::<QueuedInputReceipts>(QueuedInputReceipts::default);
        let receipts = runtime
            .0
            .get_or_try_init(|| async {
                let receipts = live
                    .queued_input_receipts()
                    .await
                    .map_err(io::Error::other)?;
                Ok::<_, io::Error>(Mutex::new(
                    receipts
                        .into_iter()
                        .map(|receipt| (receipt.id, receipt.turn_id))
                        .collect(),
                ))
            })
            .await?;
        Ok(receipts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned())
    }

    pub(super) fn known_queued_input_turn_id(&self, id: &str) -> Option<String> {
        let runtime = self
            .services
            .thread_extension_data
            .get::<QueuedInputReceipts>()?;
        let receipts = runtime.0.get()?;
        receipts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    pub(crate) async fn record_queued_tool_output(
        &self,
        turn: &TurnContext,
        model: &ModelInfo,
        item: ResponseItemEnvelope,
    ) -> io::Result<()> {
        let (items, images) = self
            .prepare_conversation_items_for_history(turn, model, vec![item])
            .await;
        self.record_prepared_items(turn, model, items, images, None)
            .await
    }

    pub(super) async fn recoverable_queued_turn(&self, turn: &TurnContext) -> bool {
        let Some(admission) = turn.extension_data.get::<QueuedInputAdmission>() else {
            return false;
        };
        match *admission.pending() {
            Some(PendingQueueInput::Accepted(_)) => return true,
            Some(PendingQueueInput::Recording(_)) => {}
            _ => return false,
        }
        // Projection can still be acknowledging an already synced append. Inspect
        // the writer-locked canonical generation, not the publication of its index.
        let Some(live) = self.live_thread() else {
            return false;
        };
        match live.queued_input_receipts().await {
            Ok(receipts) => receipts
                .iter()
                .any(|receipt| receipt.id == admission.id && receipt.turn_id == turn.sub_id),
            Err(error) => {
                tracing::warn!(turn_id = %turn.sub_id, %error, "could not verify queued input for recovery");
                false
            }
        }
    }

    pub(super) fn accept_queued_input(
        &self,
        turn: &TurnContext,
        receipt: QueuedInputReceipt,
    ) -> io::Result<()> {
        if let Some(admission) = turn.extension_data.get::<QueuedInputAdmission>()
            && admission.id == receipt.id
            && admission.accept()
        {
            Ok(())
        } else {
            self.publish_queue_receipt(receipt)
        }
    }

    /// Only release the queue row after the regular recovery checkpoint is published.
    pub(crate) async fn settle_accepted_queue_input(&self, turn: &TurnContext) {
        if let Some(admission) = turn.extension_data.get::<QueuedInputAdmission>()
            && let Some(cleanup) = admission.take_accepted()
        {
            if let Err(error) = self.publish_queue_receipt(QueuedInputReceipt {
                id: admission.id.clone(),
                turn_id: turn.sub_id.clone(),
            }) {
                self.conversation.mark_history_recovery_required();
                tracing::error!(%error, "could not publish durable queue receipt");
                return;
            }
            cleanup().await;
        }
    }

    pub(super) async fn reject_queued_input(&self, turn: &TurnContext, id: &str) -> io::Result<()> {
        let Some(admission) = turn
            .extension_data
            .get::<QueuedInputAdmission>()
            .filter(|admission| admission.id == id)
        else {
            return Err(io::Error::other(
                "missing queue admission for rejected input",
            ));
        };
        let receipt = QueuedInputReceipt {
            id: id.to_owned(),
            turn_id: turn.sub_id.clone(),
        };
        let mut write = UncertainQueueWrite::default();
        write.arm(self);
        let live = self
            .live_thread_for_persistence("record queued input veto")
            .map_err(io::Error::other)?;
        live.append_items(&[RolloutItem::QueuedInputRejected(receipt.clone())])
            .await
            .map_err(io::Error::other)?;
        self.flush_rollout().await?;
        write.confirm();
        self.publish_queue_receipt(receipt)?;
        let pending = admission.pending().take();
        if let Some(PendingQueueInput::Awaiting(cleanup)) = pending {
            cleanup().await;
        }
        Ok(())
    }

    fn publish_queue_receipt(&self, receipt: QueuedInputReceipt) -> io::Result<()> {
        let missing = || io::Error::other("queue receipt index was not initialized");
        let runtime = self
            .services
            .thread_extension_data
            .get::<QueuedInputReceipts>()
            .ok_or_else(missing)?;
        runtime
            .0
            .get()
            .ok_or_else(missing)?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(receipt.id, receipt.turn_id);
        Ok(())
    }
}

/// Cancellation or a write error can leave an append in the writer's custody.
/// Only reopening canonical history may decide whether it landed.
#[derive(Default)]
pub(super) struct UncertainQueueWrite<'a>(Option<&'a Session>);

impl<'a> UncertainQueueWrite<'a> {
    pub(super) fn arm(&mut self, session: &'a Session) {
        self.0 = Some(session);
    }

    pub(super) fn confirm(&mut self) {
        self.0 = None;
    }
}

impl Drop for UncertainQueueWrite<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.0 {
            session.conversation.mark_history_recovery_required();
        }
    }
}
