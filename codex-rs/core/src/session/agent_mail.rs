//! Durable ordinary mail owns acceptance until its stable ID enters canonical history.
//! Runtime queues are disposable projections; replay never requires a frontend.

use super::session::Session;
use super::turn_context::TurnContext;
use codex_protocol::ResponseItemId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::InterAgentCommunication;
use codex_rollout::ResponseItemEnvelope;
use codex_rollout::RolloutItem;
use codex_state::AgentMailState;
use codex_thread_store::AgentMailPersistence;
use futures::FutureExt;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct AgentMailRuntime {
    pub(crate) acceptance: tokio::sync::Mutex<()>,
    received: tokio::sync::Mutex<std::collections::HashMap<ResponseItemId, MailProjection>>,
    admissions: std::sync::Mutex<tokio_util::task::TaskTracker>,
    health: std::sync::Mutex<AdmissionHealth>,
    #[cfg(test)]
    append_fault: std::sync::Mutex<Option<AgentMailAppendFault>>,
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

fn canonical_agent_mail<'a>(
    history: &'a [RolloutItem],
    communication: &InterAgentCommunication,
) -> CodexResult<Option<&'a ResponseItemEnvelope>> {
    let Some(id) = communication.id.as_ref() else {
        return Ok(None);
    };
    let expected = communication.to_model_input_item();
    let ResponseItem::AgentMessage {
        author,
        recipient,
        content,
        ..
    } = &expected
    else {
        unreachable!("ordinary communication is typed mail")
    };
    for item in history {
        let RolloutItem::ResponseItem(item) = item else {
            continue;
        };
        if item.id() != Some(id) {
            continue;
        }
        if matches!(&item.item, ResponseItem::AgentMessage {
            author: saved_author, recipient: saved_recipient, content: saved_content, ..
        } if saved_author == author && saved_recipient == recipient && saved_content == content)
        {
            return Ok(Some(item));
        }
        return Err(CodexErr::Fatal(format!(
            "canonical agent-mail identity {id} has conflicting attribution or content"
        )));
    }
    Ok(None)
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
        if !is_accepted_agent_mail(communication) {
            return Ok(true);
        }
        let id = communication.id.as_ref().expect("accepted mail has an ID");
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

    pub(crate) async fn recover_accepted_agent_mail(self: &Arc<Self>) -> CodexResult<()> {
        let acceptance = self.agent_mail.acceptance.lock().await;
        self.recover_accepted_agent_mail_locked().await?;
        drop(acceptance);
        self.maybe_start_turn_for_pending_work().await;
        Ok(())
    }

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
        let history = live_thread.load_history(true).await.map_err(|error| {
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
            if canonical_agent_mail(&history.items, &communication)?.is_some() {
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
                continue;
            }
            let mut received = self.agent_mail.received.lock().await;
            if !received.contains_key(&record.id) {
                // Reuse current execution state and any original held deadline.
                // Restart has no ACTIVE guards; in-process retries obey normal batching.
                let config = self.get_config().await;
                let execution = *self
                    .services
                    .local_agent_runtime
                    .active_subagents()
                    .borrow();
                self.input_queue
                    .enqueue_batched_mailbox_communication(
                        communication,
                        Default::default(),
                        config.multi_agent_v2.wait_agent_batch_window_ms,
                        execution,
                    )
                    .await;
                received.insert(record.id, MailProjection::Published);
            }
        }
        Ok(!publication_blocked)
    }

    fn check_agent_mail_health(&self) -> CodexResult<()> {
        let health = self
            .agent_mail
            .health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner) = AdmissionHealth::Uncertain {
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
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[cfg(test)]
    pub(super) fn set_agent_mail_append_fault(&self, fault: AgentMailAppendFault) {
        *self
            .agent_mail
            .append_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fault);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        communication: &InterAgentCommunication,
    ) -> CodexResult<bool> {
        if !is_accepted_agent_mail(communication) {
            return Ok(false);
        }
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
            if state.shutting_down {
                return Err(CodexErr::InvalidRequest(
                    "recipient is shutting down; accepted mail retained for recovery".into(),
                ));
            }
            // Registration and the shutdown flag share state exclusion. Teardown
            // cannot miss a detached operation registered after its drain snapshot.
            self.agent_mail
                .admissions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .spawn(async move {
                    let id = mail.id.as_ref().expect("accepted mail has an ID");
                    session
                        .agent_mail
                        .received
                        .lock()
                        .await
                        .insert(id.clone(), MailProjection::Admitting);
                    let result = std::panic::AssertUnwindSafe(
                        session.commit_accepted_agent_mail(&turn, &model, &mail),
                    )
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        session.fence_agent_mail(id, "admission operation panicked".into());
                        Err(CodexErr::Fatal(
                            "accepted agent-mail admission panicked; inbox retained".into(),
                        ))
                    });
                    session.agent_mail.received.lock().await.remove(id);
                    drop(boundary);
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
        communication: &InterAgentCommunication,
    ) -> CodexResult<Option<ResponseItem>> {
        let id = communication.id.as_ref().expect("accepted mail has an ID");
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
        let AgentMailState::Pending(payload) = record.state else {
            return Ok(None);
        };
        let mut accepted: InterAgentCommunication =
            serde_json::from_str(&payload).map_err(|error| {
                CodexErr::Fatal(format!("invalid accepted agent-mail payload: {error}"))
            })?;
        accepted.id = Some(record.id);
        if &accepted != communication || communication.trigger_turn {
            return Err(CodexErr::Fatal(format!(
                "accepted agent-mail identity {id} has conflicting content or mode"
            )));
        }
        let live_thread = self.live_thread().ok_or_else(|| {
            CodexErr::Fatal("accepted agent mail requires canonical recipient history".into())
        })?;
        // The owned caller already holds the communication boundary.
        let history = live_thread.load_history(true).await.map_err(|error| {
            CodexErr::Fatal(format!(
                "cannot inspect canonical agent-mail admission: {error}"
            ))
        })?;
        if canonical_agent_mail(&history.items, communication)?.is_some() {
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
        db.thread_queue()
            .admit_agent_mail(self.thread_id, id)
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!("cannot acknowledge accepted agent mail: {error:#}"))
            })?;
        Ok(Some(envelope.item))
    }
}
