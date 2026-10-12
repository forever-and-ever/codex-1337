//! Retirement changes family membership, never conversation history.
use super::*;
use crate::state::ActiveTurn;
use codex_agent_graph_store::AgentGraphStore;
use codex_agent_graph_store::ThreadSpawnEdgeStatus;
use std::collections::HashMap;
use std::collections::HashSet;

async fn retirement_descendants(
    root: ThreadId,
    graph: Option<&dyn AgentGraphStore>,
    live_children: &HashMap<ThreadId, Vec<ThreadId>>,
) -> CodexResult<Vec<ThreadId>> {
    let mut seen = HashSet::from([root]);
    let mut pending = vec![root];
    let mut descendants = Vec::new();
    while let Some(parent) = pending.pop() {
        let mut children = live_children.get(&parent).cloned().unwrap_or_default();
        if let Some(graph) = graph {
            let closed = graph
                .list_thread_spawn_children(parent, Some(ThreadSpawnEdgeStatus::Closed))
                .await
                .map_err(|err| CodexErr::Fatal(format!("cannot read agent family: {err}")))?;
            children.retain(|child| !closed.contains(child));
            children.extend(
                graph
                    .list_thread_spawn_children(parent, Some(ThreadSpawnEdgeStatus::Open))
                    .await
                    .map_err(|err| CodexErr::Fatal(format!("cannot read agent family: {err}")))?,
            );
        }
        for child in children {
            if seen.insert(child) {
                descendants.push(child);
                pending.push(child);
            }
        }
    }
    Ok(descendants)
}

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
        let graph = state.agent_graph_store();
        let mut live_children = HashMap::<ThreadId, Vec<ThreadId>>::new();
        for (parent, child) in state.list_live_thread_spawn_edges().await {
            // Saved session sources also describe independently reopened history.
            // Only current non-root membership can supply a missing live edge.
            if self
                .runtime
                .registry
                .agent_metadata_for_thread(child)
                .is_some_and(|metadata| {
                    !metadata.agent_path.as_ref().is_some_and(AgentPath::is_root)
                })
            {
                live_children.entry(parent).or_default().push(child);
            }
        }
        // Ephemeral edges exist only in live session ancestry. Traverse the union so
        // unloaded retained agents can also bridge live descendants into the family.
        let descendants = retirement_descendants(caller, graph.as_deref(), &live_children).await?;
        if caller == target || !descendants.contains(&target) {
            return Err(CodexErr::InvalidRequest(
                "Only retained descendants may be retired.".into(),
            ));
        }
        let mut ids = retirement_descendants(target, graph.as_deref(), &live_children).await?;
        ids.push(target);
        ids.sort_by_key(ToString::to_string);
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
        // A durable incoming edge hides the subtree when closed; ephemeral incoming
        // edges never existed in storage. A missing edge needs no durable mutation.
        // A failed write changes neither membership nor execution.
        if let Some(graph) = graph {
            graph
                .set_thread_spawn_edge_status(target, ThreadSpawnEdgeStatus::Closed)
                .await
                .map_err(|err| {
                    CodexErr::Fatal(format!("failed to persist agent retirement: {err}"))
                })?;
        }
        if let Some(db) = state.get_thread(caller).await?.session.state_db() {
            db.thread_queue()
                .cancel_agent_mail_recipients(&ids)
                .await
                .map_err(|error| {
                    CodexErr::Fatal(format!("cannot cancel retired agent mail: {error:#}"))
                })?;
        }
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
