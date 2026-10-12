//! Durable ordinary mail owns acceptance until its stable ID enters canonical history.
//! Runtime queues are disposable projections; replay never requires a frontend.

use super::session::Session;
use super::turn_context::TurnContext;
use codex_protocol::ResponseItemId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::ResponseItemEnvelope;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::InterAgentCommunication;
use codex_rollout::RolloutItem;
use codex_state::AgentMailState;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct AgentMailRuntime {
    pub(crate) acceptance: tokio::sync::Mutex<()>,
    received: tokio::sync::Mutex<std::collections::HashMap<ResponseItemId, MailProjection>>,
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
        let mut admitting = std::collections::HashSet::new();
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
                Some(MailProjection::Submitting | MailProjection::Projecting) => {
                    in_flight = true;
                    false
                }
                Some(MailProjection::Published) => false,
                Some(MailProjection::Admitting) => {
                    // A cancelled admission must reconcile even with later queued mail.
                    in_flight = true;
                    admitting.insert(mail.id.clone());
                    true
                }
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
            if let Some(envelope) = canonical_agent_mail(&history.items, &communication)? {
                self.repair_agent_mail_context(envelope).await;
                self.agent_mail.received.lock().await.remove(&record.id);
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
            if admitting.contains(&record.id) && self.active_turn.lock().await.is_some() {
                // Still owned by a live turn; do not publish a second projection.
                publication_blocked = true;
                continue;
            }
            let mut received = self.agent_mail.received.lock().await;
            if matches!(received.get(&record.id), Some(MailProjection::Admitting)) {
                received.remove(&record.id);
            }
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

    /// Canonical append can outlive its cancelled turn waiter. Replay the exact
    /// envelope before acknowledging admission, without minting new provenance.
    /// Caller owns the communication boundary; the state lock makes repair idempotent.
    async fn repair_agent_mail_context(&self, envelope: &ResponseItemEnvelope) {
        let mut state = self.state.lock().await;
        if state
            .history
            .raw_items()
            .any(|item| item.id() == envelope.id())
        {
            return;
        }
        state
            .current_time_reminder
            .note_recorded_items(std::slice::from_ref(&envelope.item));
        // Only validated AgentMessage reaches this helper, never truncatable tool output.
        state.history.replay_annotated_item(
            envelope,
            codex_utils_output_truncation::TruncationPolicy::Tokens(usize::MAX),
        );
    }

    pub(crate) async fn agent_mail_admission_started(&self, id: &ResponseItemId) {
        self.agent_mail
            .received
            .lock()
            .await
            .insert(id.clone(), MailProjection::Admitting);
    }

    pub(crate) async fn record_accepted_agent_mail(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        communication: &InterAgentCommunication,
    ) -> CodexResult<bool> {
        if !is_accepted_agent_mail(communication) {
            return Ok(false);
        }
        let id = communication.id.as_ref().expect("accepted mail has an ID");
        self.agent_mail_admission_started(id).await;
        let result = self
            .commit_accepted_agent_mail(turn_context, model_info, communication)
            .await;
        self.agent_mail.received.lock().await.remove(id);
        result.map(|()| true)
    }

    async fn commit_accepted_agent_mail(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        communication: &InterAgentCommunication,
    ) -> CodexResult<()> {
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
            return Ok(());
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
        let boundary = self
            .code_mode_message_tasks
            .communication_boundary
            .acquire()
            .await
            .unwrap_or_else(|_| unreachable!("communication boundary remains open"));
        let history = live_thread.load_history(true).await.map_err(|error| {
            CodexErr::Fatal(format!(
                "cannot inspect canonical agent-mail admission: {error}"
            ))
        })?;
        if let Some(envelope) = canonical_agent_mail(&history.items, communication)? {
            self.repair_agent_mail_context(envelope).await;
            db.thread_queue()
                .admit_agent_mail(self.thread_id, id)
                .await
                .map_err(|error| {
                    CodexErr::Fatal(format!("cannot acknowledge admitted agent mail: {error:#}"))
                })?;
            return Ok(());
        }
        let response = communication.to_model_input_item();
        let (items, _) = self
            .prepare_conversation_items_for_history(
                turn_context,
                model_info,
                std::slice::from_ref(&response),
            )
            .await;
        let items = items.as_ref();
        let mut envelope = ResponseItemEnvelope::new(items[0].clone());
        let (order, pending) = {
            let mut state = self.state.lock().await;
            (
                state.history.reserve_input_order(),
                self.pending_code_mode_message_recordings(),
            )
        };
        envelope.metadata.get_or_insert_default().user_input_order = Some(order);
        for mut recording in pending {
            let _ = recording.changed().await;
        }
        // Capture provenance without admitting an item to live model context before
        // its canonical write succeeds.
        {
            let state = self.state.lock().await;
            let mut staged = state.history.clone();
            staged.record_annotated_items(
                std::slice::from_mut(&mut envelope),
                model_info.truncation_policy.into(),
            );
        }
        live_thread
            .append_items(&[
                RolloutItem::InterAgentCommunicationMetadata {
                    trigger_turn: false,
                },
                RolloutItem::ResponseItem(envelope.clone()),
            ])
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!("cannot persist accepted agent mail: {error}"))
            })?;
        {
            let mut state = self.state.lock().await;
            state.current_time_reminder.note_recorded_items(items);
            state.history.record_annotated_items(
                std::slice::from_mut(&mut envelope),
                model_info.truncation_policy.into(),
            );
        }
        db.thread_queue()
            .admit_agent_mail(self.thread_id, id)
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!("cannot acknowledge accepted agent mail: {error:#}"))
            })?;
        drop(boundary);
        self.send_raw_response_items(turn_context, items).await;
        Ok(())
    }
}
