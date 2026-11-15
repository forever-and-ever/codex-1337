//! Durable ordinary mail owns acceptance until its stable ID enters canonical history.
//! Runtime queues are disposable projections; replay never requires a frontend.

use super::session::Session;
use super::turn_context::TurnContext;
use crate::TurnStartOptions;
use crate::agent_communication::MessageDelivery;
use codex_extension_api::MessageLifecycleContributor;
use codex_protocol::ResponseItemId;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::InterAgentCommunication;
use codex_rollout::ResponseItemEnvelope;
use codex_state::AgentMailState;
use codex_thread_store::AgentMailPersistence;
use futures::FutureExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

// TELOS(accepted-agent-mail): Acceptance, canonical admission and uncertain writes remain distinct obligations.

#[derive(Default)]
pub(crate) struct AgentMailRuntime {
    pub(crate) acceptance: tokio::sync::Mutex<()>,
    pub(crate) publications: MailPublications,
    received: tokio::sync::Mutex<HashMap<ResponseItemId, MailProjection>>,
    admissions: Mutex<tokio_util::task::TaskTracker>,
    health: Mutex<AdmissionHealth>,
    #[cfg(test)]
    append_fault: Mutex<Option<AgentMailAppendFault>>,
}

/// Only observer attachments live here; the database owns acceptance.
/// Publication and removal require the recipient's acceptance gate.
pub(crate) struct MailPublications(Mutex<Option<HashMap<ResponseItemId, MessageDelivery>>>);

impl Default for MailPublications {
    fn default() -> Self {
        Self(Mutex::new(Some(HashMap::new())))
    }
}

impl MailPublications {
    pub(crate) fn publish(
        &self,
        sender: ThreadId,
        id: &ResponseItemId,
        observers: &[Arc<dyn MessageLifecycleContributor>],
    ) -> Option<MessageDelivery> {
        if let Some(delivery) = self.get(id) {
            return Some(delivery);
        }
        if observers.is_empty()
            || self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none()
        {
            return None;
        }
        let delivery = MessageDelivery::publish_durable(sender, id.clone(), observers);
        // Shutdown may close ownership while an observer captures its attachment.
        let replaced = {
            let mut publications = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(pending) = publications.as_mut() else {
                drop(publications);
                delivery.discard();
                return None;
            };
            pending.insert(id.clone(), delivery.clone())
        };
        drop(replaced);
        Some(delivery)
    }

    fn get(&self, id: &ResponseItemId) -> Option<MessageDelivery> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|publications| publications.get(id))
            .cloned()
    }

    fn remove(&self, id: &ResponseItemId) {
        let publication = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .and_then(|publications| publications.remove(id));
        drop(publication);
    }

    pub(super) fn close(&self) {
        let publications = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(publications) = publications {
            for delivery in publications.into_values() {
                delivery.discard();
            }
        }
    }
}

#[cfg(test)]
pub(super) enum AgentMailAppendFault {
    MetadataAfterCommit,
    UncertainAfterCommit,
}

#[derive(Default)]
enum AdmissionHealth {
    #[default]
    Ready,
    Uncertain {
        id: ResponseItemId,
        reason: String,
    },
}

enum MailProjection {
    Submitting,
    Projecting,
    Published,
    Admitting,
}

pub(crate) fn is_accepted_agent_mail(communication: &InterAgentCommunication) -> bool {
    communication
        .id
        .as_ref()
        .is_some_and(|id| id.as_str().starts_with("amsg_mail_"))
}

pub(super) fn accepted_mail_submission_id(op: &codex_protocol::protocol::Op) -> Option<String> {
    match op {
        codex_protocol::protocol::Op::InterAgentCommunication { communication, .. }
            if is_accepted_agent_mail(communication) =>
        {
            communication.id.as_ref().map(ToString::to_string)
        }
        _ => None,
    }
}

fn canonical_agent_mail(
    history: &HashMap<ResponseItemId, ResponseItemEnvelope>,
    communication: &InterAgentCommunication,
) -> CodexResult<bool> {
    let Some(envelope) = communication.id.as_ref().and_then(|id| history.get(id)) else {
        return Ok(false);
    };
    codex_thread_store::validate_accepted_agent_mail(envelope, communication)
        .map_err(|error| CodexErr::Fatal(error.to_string()))?;
    Ok(true)
}

impl Session {
    pub(crate) async fn reserve_agent_mail_submission(&self, id: &ResponseItemId) -> bool {
        let mut received = self.agent_mail.received.lock().await;
        if received.contains_key(id) {
            return false;
        }
        received.insert(id.clone(), MailProjection::Submitting);
        true
    }

    pub(crate) async fn release_agent_mail_submission(&self, id: &ResponseItemId) {
        let mut received = self.agent_mail.received.lock().await;
        if matches!(received.get(id), Some(MailProjection::Submitting)) {
            received.remove(id);
        }
    }

    pub(crate) async fn receive_accepted_agent_mail(
        &self,
        communication: &InterAgentCommunication,
    ) -> CodexResult<bool> {
        let Some(id) = communication
            .id
            .as_ref()
            .filter(|_| is_accepted_agent_mail(communication))
        else {
            return Ok(true);
        };
        let db = self
            .state_db()
            .ok_or_else(|| CodexErr::Fatal("accepted agent-mail database is unavailable".into()))?;
        let record = db
            .thread_queue()
            .agent_mail(self.thread_id, id)
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!("cannot read accepted agent mail: {error:#}"))
            })?
            .ok_or_else(|| {
                CodexErr::Fatal(format!("accepted agent-mail identity {id} disappeared"))
            })?;
        let AgentMailState::Pending(payload) = record.state else {
            return Ok(false);
        };
        let mut accepted: InterAgentCommunication =
            serde_json::from_str(&payload).map_err(|error| {
                CodexErr::Fatal(format!("invalid accepted agent-mail payload: {error}"))
            })?;
        accepted.id = Some(record.id);
        if &accepted != communication {
            return Err(CodexErr::Fatal(format!(
                "accepted agent-mail identity {id} has conflicting content"
            )));
        }
        let mut received = self.agent_mail.received.lock().await;
        if received
            .get(id)
            .is_some_and(|projection| !matches!(projection, MailProjection::Submitting))
        {
            return Ok(false);
        }
        received.insert(id.clone(), MailProjection::Projecting);
        Ok(true)
    }

    pub(crate) async fn agent_mail_projection_published(&self, id: Option<&ResponseItemId>) {
        if let Some(id) = id
            && let Some(projection) = self.agent_mail.received.lock().await.get_mut(id)
        {
            *projection = MailProjection::Published;
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "the acceptance gate serializes recovery with durable sends; recovery never reacquires it or waits for an actor to process a send"
    )]
    pub(crate) async fn recover_accepted_agent_mail(self: &Arc<Self>) -> CodexResult<()> {
        let acceptance = self.agent_mail.acceptance.lock().await;
        self.recover_accepted_agent_mail_locked().await?;
        drop(acceptance);
        self.maybe_start_turn_for_pending_work().await;
        Ok(())
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "idle queue checks and enqueue must be atomic with projection marks and turn admission; mailbox operations only take their private queue lock and never acquire received or active_turn"
    )]
    pub(crate) async fn recover_accepted_agent_mail_locked(&self) -> CodexResult<bool> {
        self.check_agent_mail_health()?;
        let Some(db) = self.state_db() else {
            // No durable send can be accepted by this runtime.
            return Ok(true);
        };
        let mut pending = db
            .thread_queue()
            .pending_agent_mail(self.thread_id)
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!("cannot recover accepted agent mail: {error:#}"))
            })?;
        let mut publication_blocked = false;
        {
            let active_turn = self.active_turn.lock().await;
            let mut received = self.agent_mail.received.lock().await;
            // A cancelled turn can abandon drained input before canonical admission.
            // Reclaim only published projections once neither a turn nor any queue owns them.
            let idle = active_turn.is_none();
            if idle
                && !self.input_queue.has_pending_mailbox_items().await
                && !self.input_queue.has_held_mailbox_items().await
            {
                received.retain(|_, projection| !matches!(projection, MailProjection::Published));
            }
            let mut in_flight = false;
            pending.retain(|mail| match received.get(&mail.id) {
                Some(
                    MailProjection::Submitting
                    | MailProjection::Projecting
                    | MailProjection::Admitting,
                ) => {
                    in_flight = true;
                    false
                }
                Some(MailProjection::Published) => false,
                None if in_flight => {
                    // Do not directly enqueue past an earlier actor submission whose
                    // batching decision is not yet known. Fresh sends defer as well.
                    publication_blocked = true;
                    false
                }
                None => true,
            });
        }
        if pending.is_empty() {
            return Ok(!publication_blocked);
        }
        let live_thread = self.live_thread().ok_or_else(|| {
            CodexErr::Fatal("accepted agent mail requires canonical recipient history".into())
        })?;
        let _boundary = self
            .code_mode_message_tasks
            .communication_boundary
            .acquire()
            .await
            .unwrap_or_else(|_| unreachable!("communication boundary remains open"));
        self.check_agent_mail_health()?;
        let ids: Vec<_> = pending.iter().map(|record| record.id.clone()).collect();
        let history = live_thread
            .find_accepted_agent_mail(&ids)
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!(
                    "cannot reconcile accepted agent-mail history: {error}"
                ))
            })?;
        for record in pending {
            if self.services.local_agent_runtime.agent_mail_family_root() != Some(record.root) {
                return Err(CodexErr::InvalidRequest(
                    "accepted mail belongs to a different family authority".into(),
                ));
            }
            let AgentMailState::Pending(payload) = record.state else {
                unreachable!()
            };
            let mut communication: InterAgentCommunication = serde_json::from_str(&payload)
                .map_err(|error| {
                    CodexErr::Fatal(format!("invalid accepted agent-mail payload: {error}"))
                })?;
            communication.id = Some(record.id.clone());
            if canonical_agent_mail(&history, &communication)? {
                if let Some(delivery) = self.agent_mail.publications.get(&record.id) {
                    delivery.consumed(self.thread_id);
                }
                // Identity proves past admission, not current context membership.
                // Owned admission or ordinary resume owns effective history;
                // compaction/rollback removal must never be undone here.
                db.thread_queue()
                    .admit_agent_mail(self.thread_id, &record.id)
                    .await
                    .map_err(|error| {
                        CodexErr::Fatal(format!(
                            "cannot acknowledge reconciled agent mail: {error:#}"
                        ))
                    })?;
                self.agent_mail.publications.remove(&record.id);
                continue;
            }
            let config = self.get_config().await;
            let mut received = self.agent_mail.received.lock().await;
            let delivery = self.agent_mail.publications.get(&record.id);
            if let std::collections::hash_map::Entry::Vacant(projection) = received.entry(record.id)
            {
                // Reuse current execution state and any original held deadline.
                // Restart has no ACTIVE guards; in-process retries obey normal batching.
                let execution = *self
                    .services
                    .local_agent_runtime
                    .active_subagents()
                    .borrow();
                self.input_queue
                    .enqueue_batched_mailbox_communication(
                        crate::PendingAgentMessage::new(communication, delivery),
                        TurnStartOptions::default(),
                        config.multi_agent_v2.wait_agent_batch_window_ms,
                        execution,
                    )
                    .await;
                projection.insert(MailProjection::Published);
            }
        }
        Ok(!publication_blocked)
    }

    fn check_agent_mail_health(&self) -> CodexResult<()> {
        let health = self
            .agent_mail
            .health
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match &*health {
            AdmissionHealth::Ready => Ok(()),
            AdmissionHealth::Uncertain { id, reason } => Err(CodexErr::Fatal(format!(
                "accepted agent-mail {id} has uncertain canonical persistence: {reason}. \
                 Model and compaction starts are blocked; authoritative inbox retained. \
                 Unload/reload or resume this saved conversation through ordinary canonical replay \
                 (restart the app server if its root cannot be unloaded)."
            ))),
        }
    }

    fn fence_agent_mail(&self, id: &ResponseItemId, reason: String) {
        *self
            .agent_mail
            .health
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = AdmissionHealth::Uncertain {
            id: id.clone(),
            reason,
        };
    }

    pub(crate) async fn check_agent_mail_task_start(&self) -> CodexResult<()> {
        let _boundary = self
            .code_mode_message_tasks
            .communication_boundary
            .acquire()
            .await
            .unwrap_or_else(|_| unreachable!("communication boundary remains open"));
        self.check_agent_mail_health()
    }

    pub(super) async fn drain_agent_mail_admissions(&self) {
        let tasks = {
            let tasks = self
                .agent_mail
                .admissions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            tasks.close();
            tasks.clone()
        };
        tasks.wait().await;
    }

    #[cfg(test)]
    pub(super) fn pending_agent_mail_admissions(&self) -> usize {
        self.agent_mail
            .admissions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    #[cfg(test)]
    pub(super) fn set_agent_mail_append_fault(&self, fault: AgentMailAppendFault) {
        *self
            .agent_mail
            .append_fault
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(fault);
    }

    #[cfg(test)]
    fn agent_mail_append_outcome_for_test(
        &self,
        outcome: codex_thread_store::ThreadStoreResult<codex_thread_store::AgentMailCommit>,
    ) -> codex_thread_store::ThreadStoreResult<codex_thread_store::AgentMailCommit> {
        let fault = self
            .agent_mail
            .append_fault
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match (outcome, fault) {
            (Ok(_), Some(AgentMailAppendFault::MetadataAfterCommit)) => {
                Ok(codex_thread_store::AgentMailCommit::MetadataPending(
                    codex_thread_store::ThreadStoreError::Internal {
                        message: "injected metadata failure after canonical commit".into(),
                    },
                ))
            }
            (Ok(_), Some(AgentMailAppendFault::UncertainAfterCommit)) => {
                Err(codex_thread_store::ThreadStoreError::Internal {
                    message: "injected uncertain writer outcome after canonical commit".into(),
                })
            }
            (outcome, _) => outcome,
        }
    }

    pub(crate) async fn record_accepted_agent_mail(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        model_info: &ModelInfo,
        communication: &crate::PendingAgentMessage,
    ) -> CodexResult<bool> {
        let Some(id) = communication
            .id
            .as_ref()
            .filter(|_| is_accepted_agent_mail(communication))
        else {
            return Ok(false);
        };
        let id = id.clone();
        // Never wait for a lifecycle writer while a caller might own a reader.
        // No operation is transferred if this nonblocking admission fails.
        let lifecycle = self
            .services
            .local_agent_runtime
            .agent_mail_admission_permit()?;
        let boundary = Arc::clone(&self.code_mode_message_tasks.communication_boundary)
            .acquire_owned()
            .await
            .unwrap_or_else(|_| unreachable!("communication boundary remains open"));
        self.check_agent_mail_health()?;
        let session = Arc::clone(self);
        let turn = Arc::clone(turn_context);
        let model = model_info.clone();
        let mail = communication.clone();
        let task = {
            let state = self.state.lock().await;
            if state.runtime_holds.is_closing() {
                return Err(CodexErr::InvalidRequest(
                    "recipient is shutting down; accepted mail retained for recovery".into(),
                ));
            }
            let runtime_hold = state
                .runtime_holds
                .acquire()
                .unwrap_or_else(|| unreachable!("state exclusion keeps admission open"));
            // Registration and the close phase share state exclusion. Teardown
            // cannot miss a detached operation registered after its drain snapshot.
            self.agent_mail
                .admissions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .spawn(async move {
                    let _runtime_hold = runtime_hold;
                    session
                        .agent_mail
                        .received
                        .lock()
                        .await
                        .insert(id.clone(), MailProjection::Admitting);
                    let result = std::panic::AssertUnwindSafe(
                        session.commit_accepted_agent_mail(&turn, &model, &mail, &id),
                    )
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        session.fence_agent_mail(&id, "admission operation panicked".into());
                        Err(CodexErr::Fatal(
                            "accepted agent-mail admission panicked; inbox retained".into(),
                        ))
                    });
                    if let Err(error) = &result {
                        session.fence_agent_mail(&id, error.to_string());
                    }
                    session.agent_mail.received.lock().await.remove(&id);
                    drop(boundary);
                    if result.is_ok() {
                        // Never take acceptance while holding the boundary: recovery
                        // owns them in the opposite order. Keep observer custody until
                        // the receipt is committed so retrying acceptance cannot publish twice.
                        let _acceptance = session.agent_mail.acceptance.lock().await;
                        session.agent_mail.publications.remove(&id);
                    }
                    drop(lifecycle);
                    result
                })
        };
        let response = task.await.map_err(|error| {
            CodexErr::Fatal(format!(
                "accepted agent-mail admission task failed: {error}"
            ))
        })??;
        // UI observation is not part of durable admission or shutdown draining.
        if let Some(response) = response {
            self.send_raw_response_items(turn_context, &[response])
                .await;
        }
        Ok(true)
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "runtime-owned admission excludes all history mutation through canonical commit and live insertion"
    )]
    async fn commit_accepted_agent_mail(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        communication: &crate::PendingAgentMessage,
        id: &ResponseItemId,
    ) -> CodexResult<Option<ResponseItem>> {
        let db = self
            .state_db()
            .ok_or_else(|| CodexErr::Fatal("accepted agent-mail database is unavailable".into()))?;
        let record = db
            .thread_queue()
            .agent_mail(self.thread_id, id)
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!(
                    "cannot read accepted agent-mail admission: {error:#}"
                ))
            })?
            .ok_or_else(|| {
                CodexErr::Fatal(format!("accepted agent-mail identity {id} disappeared"))
            })?;
        let payload = match record.state {
            AgentMailState::Pending(payload) => payload,
            AgentMailState::Admitted => {
                if let Some(delivery) = &communication.delivery {
                    delivery.consumed(self.thread_id);
                }
                return Ok(None);
            }
            AgentMailState::Cancelled => return Ok(None),
        };
        let mut accepted: InterAgentCommunication =
            serde_json::from_str(&payload).map_err(|error| {
                CodexErr::Fatal(format!("invalid accepted agent-mail payload: {error}"))
            })?;
        accepted.id = Some(record.id);
        if accepted != **communication || communication.trigger_turn {
            return Err(CodexErr::Fatal(format!(
                "accepted agent-mail identity {id} has conflicting content or mode"
            )));
        }
        let live_thread = self.live_thread().ok_or_else(|| {
            CodexErr::Fatal("accepted agent mail requires canonical recipient history".into())
        })?;
        // The owned caller already holds the communication boundary.
        let history = live_thread
            .find_accepted_agent_mail(std::slice::from_ref(id))
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!(
                    "cannot inspect canonical agent-mail admission: {error}"
                ))
            })?;
        if canonical_agent_mail(&history, communication)? {
            if let Some(delivery) = &communication.delivery {
                delivery.consumed(self.thread_id);
            }
            // Resume owns effective context; past canonical admission must never
            // resurrect an item subsequently removed by compaction or rollback.
            db.thread_queue()
                .admit_agent_mail(self.thread_id, id)
                .await
                .map_err(|error| {
                    CodexErr::Fatal(format!("cannot acknowledge admitted agent mail: {error:#}"))
                })?;
            return Ok(None);
        }
        let response = communication.to_model_input_item();
        let (items, _) = self
            .prepare_conversation_items_for_history(
                turn_context,
                model_info,
                std::slice::from_ref(&response),
            )
            .await;
        let mut envelope = ResponseItemEnvelope::new(items[0].clone());
        let (order, pending) = {
            let mut state = self.state.lock().await;
            (
                state.history.reserve_input_order(),
                self.pending_code_mode_message_recordings(),
            )
        };
        envelope.metadata.get_or_insert_default().user_input_order = Some(order);
        // Those workers own boundary -> settings too. Never wait for them under settings/state.
        for mut recording in pending {
            let _ = recording.changed().await;
        }
        let settings = super::thread_settings::acquire_persistence_lock(self).await;
        let mut state = self.state.lock().await;
        let mut staged = state.history.clone();
        staged.record_annotated_items(
            std::slice::from_mut(&mut envelope),
            model_info.truncation_policy.into(),
        );
        let outcome = live_thread.append_accepted_agent_mail(&envelope).await;
        #[cfg(test)]
        let outcome = self.agent_mail_append_outcome_for_test(outcome);
        match outcome {
            Ok(codex_thread_store::AgentMailCommit::Complete) => {}
            Ok(codex_thread_store::AgentMailCommit::MetadataPending(error)) => {
                tracing::warn!(%error, mail_id = %id,
                    "canonical agent mail committed; rebuildable thread metadata remains pending");
            }
            Err(error) => {
                self.fence_agent_mail(id, format!("{error:#}"));
                // Err is not evidence of an absent write. Keep Pending and prevent
                // all model/compaction starts until ordinary canonical resume.
                return self.check_agent_mail_health().map(|()| None);
            }
        }
        state
            .current_time_reminder
            .note_recorded_items(std::slice::from_ref(&envelope.item));
        state.history = staged;
        drop(state);
        drop(settings);
        if let Some(delivery) = &communication.delivery {
            delivery.consumed(self.thread_id);
        }
        db.thread_queue()
            .admit_agent_mail(self.thread_id, id)
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!("cannot acknowledge accepted agent mail: {error:#}"))
            })?;
        Ok(Some(envelope.item))
    }
}
