//! Retains exact V2 residency exclusion through sealed, cancellation-independent teardown.

use super::runtime::AgentTreeMembership;
use crate::CodexThread;
use crate::ThreadEvictionOutcome;
use crate::agent::registry::EvictedEnvironments;
use crate::thread_manager::IdleCloseEligibility;
use crate::thread_manager::ThreadManagerState;
use codex_protocol::error::CodexErrKind;
use codex_protocol::error::Result as CodexResult;
use std::sync::Arc;

impl ThreadManagerState {
    pub(crate) async fn evict_idle_v2(
        self: &Arc<Self>,
        thread: Arc<CodexThread>,
        membership: &AgentTreeMembership,
        eligibility: IdleCloseEligibility,
    ) -> CodexResult<ThreadEvictionOutcome> {
        let Ok(residency_guard) = Arc::clone(&thread.residency_gate).try_write_owned() else {
            return Ok(ThreadEvictionOutcome::Contended);
        };
        let thread_id = thread.session.thread_id;
        if !self
            .get_thread(thread_id)
            .await
            .is_ok_and(|registered| Arc::ptr_eq(&registered, &thread))
        {
            return Ok(ThreadEvictionOutcome::NotCurrent);
        }
        match self.seal_idle_thread(
            &thread,
            &eligibility,
            &crate::session::IdleMailboxPolicy::RetainQueued,
        ) {
            crate::runtime_holds::IdleSeal::Sealed => {}
            crate::runtime_holds::IdleSeal::Busy => return Ok(ThreadEvictionOutcome::Busy),
            crate::runtime_holds::IdleSeal::Contended => {
                return Ok(ThreadEvictionOutcome::Contended);
            }
        }
        let manager = Arc::clone(self);
        let teardown = membership
            .clone()
            .into_teardown_guard("resident_eviction", Some(thread_id));
        // Once shutdown is submitted, cancellation cannot release delivery or capacity early.
        tokio::spawn(async move {
            let _residency_guard = residency_guard;
            thread.ensure_rollout_materialized().await;
            if let Err(err) = thread.shutdown_and_wait().await {
                teardown.record_shutdown_failure("stop_resident", CodexErrKind::from(&err).into());
                teardown.complete();
                return Err(err);
            }
            // Dispatch has stopped and the residency guard excludes senders. A tree
            // shutdown deliberately discards its mailbox instead of retaining it.
            let mail = thread
                .session
                .input_queue
                .drain_mailbox()
                .await
                .into_iter()
                .map(|mail| mail.communication)
                // Accepted mail keeps its durable owner; replay restores it after reload.
                .filter(|mail| !crate::session::agent_mail::is_accepted_agent_mail(mail))
                .collect();
            let runtime = &thread.session.services.local_agent_runtime;
            let _ = runtime.mailboxes.enqueue(thread_id, /*id*/ None, mail);
            // Preserve configured bindings across eviction without retaining turn-only attachments.
            // Raw bindings keep the original pending owner configuration alive.
            let (config, ..) = Box::pin(thread.session.fork_config()).await;
            let environments = EvictedEnvironments {
                config: Arc::new(config),
                has_independent_service_tier: matches!(
                    &thread.session.agent_runtime,
                    crate::agent::host::AgentRuntimeServices::Host(services) if services.service_tier.is_none()
                ),
                selections: thread.environment_selections().await,
                bindings: thread
                    .session
                    .services
                    .turn_environments
                    .configured_snapshot_now(),
            };
            let mut threads = manager.threads.write().await;
            if threads
                .get(&thread_id)
                .is_some_and(|registered| !Arc::ptr_eq(registered, &thread))
            {
                teardown.complete();
                return Ok(ThreadEvictionOutcome::NotCurrent);
            }
            runtime
                .registry
                .save_evicted_environments(thread_id, environments);
            let removed = threads.remove(&thread_id).is_some();
            runtime.residency.remove(thread_id);
            teardown.complete();
            Ok(if removed {
                ThreadEvictionOutcome::Evicted
            } else {
                ThreadEvictionOutcome::NotCurrent
            })
        })
        .await?
    }
}
