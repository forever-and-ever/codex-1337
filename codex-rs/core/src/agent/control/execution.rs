//! Tracks local running capacity and releases reservations through the shared guard.
//! The local permit owns the running count; root and MAv1 turns remain unrestricted.

use super::LocalAgentControl;
use super::runtime::LocalAgentRuntime;
use crate::agent::types::AgentExecutionGuard;
use codex_protocol::error::AgentErrorContext;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::watch;

#[derive(Clone, Copy, Default)]
pub(crate) struct FamilyExecution {
    pub(crate) active: usize,
    /// Remembers eligibility even when watch coalesces a quick count drop and restart.
    pub(crate) release_generation: u64,
}

#[derive(Default)]
pub(super) struct AgentExecutionLimiter {
    active: watch::Sender<FamilyExecution>,
    max_threads: OnceLock<usize>,
}

struct LocalExecutionPermit {
    limiter: Arc<AgentExecutionLimiter>,
}

impl Drop for LocalExecutionPermit {
    fn drop(&mut self) {
        self.limiter.active.send_modify(|state| {
            state.active -= 1;
            if state.active < 2 {
                state.release_generation += 1;
            }
        });
    }
}

impl LocalAgentControl {
    pub(crate) fn ensure_execution_capacity(
        &self,
        multi_agent_version: MultiAgentVersion,
        session_source: &SessionSource,
    ) -> CodexResult<()> {
        if !is_execution_limited(multi_agent_version, session_source) {
            return Ok(());
        }
        let max_threads = self.runtime.agent_execution_limiter.max_threads();
        if self.runtime.agent_execution_limiter.has_capacity() {
            Ok(())
        } else {
            Err(
                CodexErr::new(CodexErrorDetails::AgentLimitReached { max_threads })
                    .with_agent_context(AgentErrorContext::ExecutionCapacity),
            )
        }
    }

    pub(crate) fn execution_guard(
        &self,
        multi_agent_version: MultiAgentVersion,
        session_source: &SessionSource,
    ) -> Option<AgentExecutionGuard> {
        is_execution_limited(multi_agent_version, session_source)
            .then(|| Arc::clone(&self.runtime.agent_execution_limiter).guard())
    }
}

impl AgentExecutionLimiter {
    pub(super) fn initialize(&self, max_threads: usize) {
        self.max_threads.get_or_init(|| max_threads);
    }

    fn max_threads(&self) -> usize {
        self.max_threads.get().copied().unwrap_or(usize::MAX)
    }

    fn has_capacity(&self) -> bool {
        self.active.borrow().active < self.max_threads()
    }

    fn guard(self: Arc<Self>) -> AgentExecutionGuard {
        self.active.send_modify(|state| state.active += 1);
        AgentExecutionGuard::new(LocalExecutionPermit { limiter: self })
    }
}

impl LocalAgentRuntime {
    pub(crate) fn active_subagents(&self) -> watch::Receiver<FamilyExecution> {
        self.agent_execution_limiter.active.subscribe()
    }
}

fn is_execution_limited(
    multi_agent_version: MultiAgentVersion,
    session_source: &SessionSource,
) -> bool {
    multi_agent_version == MultiAgentVersion::V2
        && matches!(session_source, SessionSource::SubAgent(_))
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
