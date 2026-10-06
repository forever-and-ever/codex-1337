//! Retirement changes durable family membership, never conversation history.
use super::*;
use crate::state::ActiveTurn;
use codex_agent_graph_store::ThreadSpawnEdgeStatus;

impl LocalAgentControl {
    /// Retire an idle descendant subtree. Accepted retirement finishes even if its caller leaves.
    pub(crate) async fn retire_agent(
        &self,
        caller: ThreadId,
        target: ThreadId,
    ) -> CodexResult<Vec<ThreadId>> {
        let control = self.clone();
        tokio::spawn(async move {
            let state = control.runtime.upgrade()?;
            let ids = control.retire_subtree(caller, target).await?;
            if !ids.is_empty() {
                state.notify_threads_retired(ids.clone());
            }
            Ok(ids)
        })
        .await
        .map_err(|err| CodexErr::Fatal(format!("agent retirement failed: {err}")))?
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "Seal idle turn admission until retirement is persisted."
    )]
    async fn retire_subtree(
        &self,
        caller: ThreadId,
        target: ThreadId,
    ) -> CodexResult<Vec<ThreadId>> {
        let _lifecycle = self.runtime.lifecycle.write().await;
        let state = self.runtime.upgrade()?;
        let graph = state.agent_graph_store().ok_or_else(|| {
            CodexErr::InvalidRequest("agent retirement requires persistent storage".into())
        })?;
        let descendants = graph
            .list_thread_spawn_descendants(caller, Some(ThreadSpawnEdgeStatus::Open))
            .await
            .map_err(|err| CodexErr::Fatal(format!("cannot read agent family: {err}")))?;
        if caller == target || !descendants.contains(&target) {
            return Err(CodexErr::InvalidRequest(
                "Only retained descendants may be retired.".into(),
            ));
        }
        let mut ids = graph
            .list_thread_spawn_descendants(target, Some(ThreadSpawnEdgeStatus::Open))
            .await
            .map_err(|err| CodexErr::Fatal(format!("cannot read agent subtree: {err}")))?;
        ids.push(target);
        ids.extend(self.runtime.live_thread_spawn_descendants(target).await?);
        ids.sort_by_key(ToString::to_string);
        ids.dedup();
        let mut threads = Vec::new();
        for id in &ids {
            if let Ok(thread) = state.get_thread(*id).await {
                threads.push(thread);
            }
        }
        let mut admission = Vec::new();
        for thread in &threads {
            let guard = thread.session.active_turn.lock().await;
            if guard.is_some()
                || thread
                    .session
                    .input_queue
                    .has_trigger_turn_mailbox_items()
                    .await
            {
                return Err(CodexErr::InvalidRequest(
                    "Subagent or descendant is working; interrupt it before retiring.".into(),
                ));
            }
            admission.push(guard);
        }
        for thread in &threads {
            thread.ensure_rollout_materialized().await;
            thread.flush_rollout().await?;
        }
        // Closing the incoming edge removes the entire subtree from open-edge traversals.
        // A failed write changes neither membership nor execution.
        graph
            .set_thread_spawn_edge_status(target, ThreadSpawnEdgeStatus::Closed)
            .await
            .map_err(|err| CodexErr::Fatal(format!("failed to persist agent retirement: {err}")))?;
        for id in &ids {
            self.runtime.registry.release_spawned_thread(*id);
            self.forget_v2_residency(*id);
        }
        // Reserve idle slots until Shutdown consumes them. Background task admission must not
        // start work between releasing these locks and processing the shutdown submission.
        for guard in &mut admission {
            **guard = Some(ActiveTurn::default());
        }
        drop(admission);
        for id in &ids {
            state.remove_thread(id).await;
        }
        drop(_lifecycle);
        for thread in threads {
            let id = thread.session.thread_id;
            if let Err(error) = thread.shutdown_and_wait().await {
                warn!(%id, %error, "retired agent was already unavailable during shutdown");
            }
        }
        Ok(ids)
    }
}
