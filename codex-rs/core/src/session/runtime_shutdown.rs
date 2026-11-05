//! Explicit runtime teardown cancels dependency owners before joining any of them.

use super::session::Session;
use crate::thread_manager::AgentTreeShutdownFailure;

pub(super) fn begin(session: &Session) {
    session.services.code_mode_service.begin_shutdown();
    session
        .services
        .unified_exec_manager
        .begin_shutdown_processes();
}

pub(super) async fn drain(session: &Session) {
    // Start cell-session closure before joining launches. A launch can still be
    // awaiting an approval owned by a yielded cell, with no parent turn/backend.
    let (code_mode, ()) = tokio::join!(
        session.services.code_mode_service.shutdown(),
        session.services.unified_exec_manager.shutdown_processes(),
    );
    if let Err(error) = code_mode {
        session
            .services
            .local_agent_runtime
            .record_shutdown_failure(AgentTreeShutdownFailure::operation_failed(
                "session_shutdown",
                "code_mode",
                Some(session.thread_id),
                "code_mode_error",
            ));
        tracing::warn!(%error, "failed to shutdown code mode session");
    }
}
