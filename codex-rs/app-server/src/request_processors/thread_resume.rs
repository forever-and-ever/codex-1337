use super::ThreadRequestProcessor;
use super::invalid_request;
use crate::outgoing_message::ConnectionRequestId;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_transport::daemon_recovery::InterruptedTurn;
use codex_core::CodexThread;
use codex_extension_api::ThreadIdleCause;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentStatus;
use std::ops::ControlFlow;
use std::sync::Arc;

pub(crate) enum ThreadResumeTarget {
    Client(ConnectionRequestId),
    Server(ServerResume),
}

pub(crate) enum ServerResume {
    Coordination,
    DaemonRecovery(Option<InterruptedTurn>),
}

impl ServerResume {
    pub(super) async fn reuse_runtime(
        &self,
        processor: &ThreadRequestProcessor,
        thread_id: &str,
    ) -> Result<ControlFlow<()>, JSONRPCErrorError> {
        let id = ThreadId::from_string(thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        // The caller holds the same permit as client resume, including after config loading.
        if let Ok(thread) = processor.thread_manager.get_thread(id).await {
            if matches!(self, Self::Coordination)
                && matches!(thread.agent_status().await, AgentStatus::Shutdown)
            {
                let _ = processor
                    .thread_manager
                    .remove_thread_if_matches(&id, &thread)
                    .await;
            } else {
                if let Self::DaemonRecovery(Some(saved)) = self {
                    processor
                        .continue_daemon_turn(thread_id, saved.clone())
                        .await;
                }
                return Ok(ControlFlow::Break(()));
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    pub(super) async fn observe_runtime(
        &self,
        processor: &ThreadRequestProcessor,
        thread_id: ThreadId,
        thread: &Arc<CodexThread>,
    ) -> Result<(), JSONRPCErrorError> {
        // Observe lifecycle events without attaching a client subscription.
        processor
            .thread_watch_manager
            .upsert_thread(&thread_id.to_string())
            .await;
        // Invoke idle work before arming subscriber-based unloading.
        thread
            .emit_thread_idle_lifecycle_if_idle(ThreadIdleCause::Completed)
            .await;
        if let Self::DaemonRecovery(Some(saved)) = self {
            processor
                .continue_daemon_turn(&thread_id.to_string(), saved.clone())
                .await;
        }
        let state = processor.thread_state_manager.thread_state(thread_id).await;
        processor
            .ensure_listener_task_running(thread_id, Arc::clone(thread), state)
            .await
    }
}
