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
use codex_protocol::error::CodexErr;
use codex_protocol::openai_models::ModelInfo;
use tokio::sync::OnceCell;

use super::Session;
use super::TurnContext;
use super::TurnInput;
use super::UserInputMetadata;

type QueueCleanup = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

pub(crate) struct QueuedInputAdmission {
    pub(crate) id: String,
    cleanup: Mutex<Option<QueueCleanup>>,
}

impl QueuedInputAdmission {
    pub(crate) fn new<F>(id: String, cleanup: impl FnOnce() -> F + Send + 'static) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self {
            id,
            cleanup: Mutex::new(Some(Box::new(move || Box::pin(cleanup())))),
        }
    }

    fn take_cleanup(&self) -> Option<QueueCleanup> {
        self.cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

pub(crate) async fn complete_queued_input(turn: &TurnContext, id: &str) {
    if let Some(admission) = turn.extension_data.get::<QueuedInputAdmission>()
        && admission.id == id
        && let Some(cleanup) = admission.take_cleanup()
    {
        cleanup().await;
    }
}

pub(crate) fn cancel_unrecorded_queued_input(turn: &TurnContext) {
    if let Some(admission) = turn.extension_data.get::<QueuedInputAdmission>() {
        drop(admission.take_cleanup());
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

pub(crate) fn leave_unrecorded_in_queue(input: &mut Vec<TurnInput>) {
    input.retain(|item| queued_input_id(item).is_none());
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

    pub(super) async fn confirm_queued_input(
        &self,
        turn_context: &TurnContext,
        receipt: QueuedInputReceipt,
    ) {
        let runtime = self
            .services
            .thread_extension_data
            .get::<QueuedInputReceipts>()
            .expect("queued input reconciliation precedes turn admission");
        runtime
            .0
            .get()
            .expect("queue receipt index was initialized")
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(receipt.id.clone(), receipt.turn_id);
        complete_queued_input(turn_context, &receipt.id).await;
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
