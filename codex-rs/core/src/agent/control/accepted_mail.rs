//! Ordinary-mail acceptance commits before submission acknowledgment.
//! An accepted ID identifies canonical admission, not provider observation or tool effects.

use super::LocalAgentControl;
use super::LocalAgentRuntime;
use crate::TurnStartOptions;
use crate::agent_communication::AgentCommunicationContext;
use crate::thread_manager::ThreadManagerState;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_state::AgentMailState;
use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;
use tracing::warn;

impl LocalAgentControl {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "retirement must wait for runtime-owned acceptance; commit only publishes an actor submission and never waits for retirement or reacquires lifecycle"
    )]
    pub(super) async fn submit_accepted_agent_mail(
        &self,
        recipient: ThreadId,
        manager: &Arc<ThreadManagerState>,
        communication: InterAgentCommunication,
        context: AgentCommunicationContext,
        start_options: TurnStartOptions,
    ) -> CodexResult<String> {
        let control = self.clone();
        let manager = Arc::clone(manager);
        // Once started, acceptance/publication belongs to the runtime, not the
        // sender's cancellable tool future. Retirement waits for this guard.
        tokio::spawn(async move {
            let _lifecycle = control.runtime.lifecycle.read().await;
            if control.runtime.shutdown.is_cancelled() {
                return Err(CodexErr::InvalidRequest(
                    "agent runtime is shutting down".into(),
                ));
            }
            control
                .commit_accepted_agent_mail_submission(
                    recipient,
                    &manager,
                    communication,
                    context,
                    start_options,
                )
                .await
        })
        .await
        .map_err(|error| {
            CodexErr::Fatal(format!("ordinary agent-mail acceptance failed: {error}"))
        })?
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "acceptance order must include durable commit and actor publication; this path never reacquires acceptance or waits for actor processing"
    )]
    async fn commit_accepted_agent_mail_submission(
        &self,
        recipient: ThreadId,
        manager: &Arc<ThreadManagerState>,
        mut communication: InterAgentCommunication,
        context: AgentCommunicationContext,
        start_options: TurnStartOptions,
    ) -> CodexResult<String> {
        let thread = manager.get_thread(recipient).await?;
        // Serialize durable order with publication for this recipient. Recovery uses
        // the same gate before publishing older accepted messages.
        let _acceptance = thread.session.agent_mail.acceptance.lock().await;
        let predecessors_published = thread.session.recover_accepted_agent_mail_locked().await?;
        let db = thread.session.state_db().ok_or_else(|| {
            CodexErr::Fatal(
                "ordinary agent-mail acceptance requires the authoritative queue database".into(),
            )
        })?;
        if thread.session.live_thread().is_none() {
            return Err(CodexErr::UnsupportedOperation(
                "durable ordinary mail requires a saved recipient conversation".into(),
            ));
        }
        thread
            .session
            .try_ensure_rollout_materialized(codex_thread_store::PersistContext::Standard)
            .await?;
        let root = self
            .runtime
            .registry
            .agent_id_for_path(&AgentPath::root())
            .ok_or_else(|| {
                CodexErr::InvalidRequest("agent family root is not registered".into())
            })?;
        let id =
            (thread.session.item_id_generator)(codex_thread_store::ACCEPTED_AGENT_MAIL_ID_KIND);
        let origin = context.durable_origin(id.as_str());
        communication.id = None;
        let payload = serde_json::to_string(&communication).map_err(|error| {
            CodexErr::Fatal(format!("cannot encode accepted agent mail: {error}"))
        })?;
        let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
        let record = db
            .thread_queue()
            .accept_agent_mail(
                &id,
                root,
                context.sender_thread_id,
                recipient,
                &origin,
                &digest,
                &payload,
            )
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!("cannot accept ordinary agent mail: {error:#}"))
            })?;
        match record.state {
            AgentMailState::Admitted => return Ok(record.id.to_string()),
            AgentMailState::Cancelled => {
                return Err(CodexErr::InvalidRequest(
                    "this agent-mail origin was cancelled by conversation retirement or deletion"
                        .into(),
                ));
            }
            AgentMailState::Pending(_) => {}
        }
        communication.id = Some(record.id.clone());
        let delivery = thread.session.agent_mail.publications.publish(
            context.sender_thread_id,
            &record.id,
            manager.extensions.message_lifecycle_contributors(),
        );
        if !predecessors_published {
            // An older row awaits an actor's batching decision before its orphaned
            // successor can publish. Retain this acceptance behind that obligation.
            return Ok(record.id.to_string());
        }
        if !thread
            .session
            .reserve_agent_mail_submission(&record.id)
            .await
        {
            return Ok(record.id.to_string());
        }
        if let Err(error) = manager
            .send_op(
                recipient,
                Op::InterAgentCommunication {
                    communication: communication.clone(),
                    start_options,
                },
                None,
                None,
                delivery,
            )
            .await
        {
            thread
                .session
                .release_agent_mail_submission(&record.id)
                .await;
            // The committed obligation survives this runtime. Server reconciliation
            // retries publication; returning rejection here would lie about acceptance.
            warn!(%recipient, %error, mail_id = %record.id, "accepted agent mail awaits recipient recovery");
        }
        crate::agent_communication::emit_agent_communication_send(
            record.id.as_str(),
            &context,
            crate::agent_communication::send_log_content(&communication).as_deref(),
            recipient,
        );
        Ok(record.id.to_string())
    }
}

impl LocalAgentRuntime {
    /// Nonblocking acquisition avoids a queued writer / caller-owned reader inversion.
    /// Acquire before registering an admission task that session teardown will drain.
    pub(crate) fn agent_mail_admission_permit(
        &self,
    ) -> CodexResult<tokio::sync::OwnedRwLockReadGuard<()>> {
        Arc::clone(&self.lifecycle).try_read_owned().map_err(|_| {
            CodexErr::InvalidRequest(
                "agent family lifecycle is changing; accepted mail retained for recovery".into(),
            )
        })
    }

    pub(crate) fn agent_mail_family_root(&self) -> Option<ThreadId> {
        self.registry.agent_id_for_path(&AgentPath::root())
    }

    pub(crate) async fn recover_accepted_agent_mail(
        &self,
        root: ThreadId,
        recipient: ThreadId,
    ) -> CodexResult<()> {
        let manager = self.upgrade()?;
        let root_thread = manager.get_thread(root).await?;
        let control = LocalAgentControl {
            session_id: root_thread.session.services.agent_control.identity(),
            runtime: self.clone(),
        };
        if self.registry.agent_id_for_path(&AgentPath::root()) != Some(root) {
            return Err(CodexErr::InvalidRequest(
                "accepted mail family authority does not match the loaded root".into(),
            ));
        }
        if recipient != root
            && manager.get_thread(recipient).await.is_err()
            && let Some(graph) = manager.agent_graph_store()
        {
            // Absence is not retirement: rebuilding metadata can temporarily lose
            // an open edge. Only an explicit closed edge may cancel accepted mail.
            let mut child = recipient;
            let mut seen = std::collections::HashSet::new();
            let mut retired = false;
            while child != root {
                if !seen.insert(child) {
                    return Err(CodexErr::Fatal(
                        "accepted-mail family ancestry contains a cycle".into(),
                    ));
                }
                let stored = manager
                    .read_stored_thread(codex_thread_store::ReadThreadParams {
                        thread_id: child,
                        include_archived: true,
                        include_history: false,
                    })
                    .await?;
                let parent = stored.parent_thread_id.ok_or_else(|| {
                    CodexErr::InvalidRequest(
                        "accepted-mail recipient has no recorded path to its original family"
                            .into(),
                    )
                })?;
                let closed = graph
                    .list_thread_spawn_children(
                        parent,
                        Some(codex_agent_graph_store::ThreadSpawnEdgeStatus::Closed),
                    )
                    .await
                    .map_err(|error| {
                        CodexErr::Fatal(format!(
                            "cannot recover agent-mail family membership: {error}"
                        ))
                    })?;
                if closed.contains(&child) {
                    retired = true;
                    break;
                }
                child = parent;
            }
            if retired {
                let db = root_thread.session.state_db().ok_or_else(|| {
                    CodexErr::Fatal("authoritative agent-mail inbox is unavailable".into())
                })?;
                db.thread_queue()
                    .cancel_agent_mail_recipients(&[recipient])
                    .await
                    .map_err(|error| {
                        CodexErr::Fatal(format!("cannot cancel retired recipient mail: {error:#}"))
                    })?;
                return Ok(());
            }
        }
        let config = root_thread.session.get_config().await;
        let _residency =
            Box::pin(control.ensure_v2_agent_loaded((*config).clone(), recipient, None)).await?;
        let thread = manager.get_thread(recipient).await?;
        thread.session.recover_accepted_agent_mail().await?;
        Ok(())
    }
}
