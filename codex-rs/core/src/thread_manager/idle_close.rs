//! Automatic close seals exact-runtime admission before cancellation-independent cleanup.
//! Capacity deliberately skips the timer, never the holds/viewer check.

use super::ThreadManager;
use super::ThreadManagerState;
use crate::CodexThread;
use crate::ThreadEvictionOutcome;
use crate::runtime_holds::RuntimeActivity;
use crate::runtime_holds::RuntimeHolds;
use codex_protocol::error::Result as CodexResult;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

/// A quiet snapshot belonging to one runtime. Sealing revalidates its revision.
pub struct IdleCloseEligibility {
    runtime: RuntimeHolds,
    revision: u64,
}

impl CodexThread {
    pub fn idle_close_eligibility(&self, delay: Duration) -> Option<IdleCloseEligibility> {
        let runtime = self.runtime_holds();
        match runtime.activity() {
            RuntimeActivity::Quiet { since, revision }
                if since
                    .checked_add(delay)
                    .is_some_and(|deadline| deadline <= Instant::now()) =>
            {
                Some(IdleCloseEligibility { runtime, revision })
            }
            _ => None,
        }
    }
}

impl ThreadManager {
    /// Returns Busy rather than waiting for admission. Once sealed, cleanup owns
    /// its gate independently of this observer's cancellation.
    pub async fn try_idle_close(
        &self,
        thread: Arc<CodexThread>,
        eligibility: IdleCloseEligibility,
    ) -> CodexResult<ThreadEvictionOutcome> {
        if thread.multi_agent_version() == Some(codex_protocol::protocol::MultiAgentVersion::V2)
            && matches!(
                thread.session_source,
                codex_protocol::protocol::SessionSource::SubAgent(_)
            )
        {
            if self.state.agent_control_factory.is_some() {
                return Err(codex_protocol::error::CodexErr::InvalidRequest(
                    "idle eviction requires the local agent controller".to_owned(),
                ));
            }
            let Ok(membership) = thread.session.services.local_agent_runtime.admit_start() else {
                return Ok(ThreadEvictionOutcome::Busy);
            };
            return self
                .state
                .evict_idle_v2(thread, &membership, eligibility)
                .await;
        }
        let Ok(gate) = Arc::clone(&thread.residency_gate).try_write_owned() else {
            return Ok(ThreadEvictionOutcome::Contended);
        };
        match self.state.seal_idle_thread(
            &thread,
            &eligibility,
            &crate::session::IdleMailboxPolicy::Empty,
        ) {
            crate::runtime_holds::IdleSeal::Sealed => {}
            crate::runtime_holds::IdleSeal::Busy => return Ok(ThreadEvictionOutcome::Busy),
            crate::runtime_holds::IdleSeal::Contended => {
                return Ok(ThreadEvictionOutcome::Contended);
            }
        }
        let threads = self.state.threads.clone();
        tokio::spawn(async move {
            let _gate = gate;
            thread.shutdown_and_wait().await?;
            let mut threads = threads.write().await;
            let id = thread.session.thread_id;
            if threads
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, &thread))
            {
                threads.remove(&id);
                Ok(ThreadEvictionOutcome::Evicted)
            } else {
                Ok(ThreadEvictionOutcome::NotCurrent)
            }
        })
        .await?
    }
}

impl ThreadManagerState {
    pub(crate) fn seal_idle_thread(
        &self,
        thread: &Arc<CodexThread>,
        eligibility: &IdleCloseEligibility,
        mailbox: &crate::session::IdleMailboxPolicy,
    ) -> crate::runtime_holds::IdleSeal {
        use crate::runtime_holds::IdleSeal;
        let Ok(threads) = self.threads.try_read() else {
            return IdleSeal::Contended;
        };
        if !threads
            .get(&thread.session.thread_id)
            .is_some_and(|current| Arc::ptr_eq(current, thread))
            || !thread.runtime_holds().same_runtime(&eligibility.runtime)
        {
            return IdleSeal::Busy;
        }
        thread
            .session
            .seal_idle_runtime(eligibility.revision, mailbox)
    }
}
