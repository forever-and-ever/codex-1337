//! Wait attribution follows the process store's original invocation, never its rendered command.

use super::UnifiedExecContext;
use super::UnifiedExecProcessManager;
use super::WriteStdinRequest;
use crate::tools::code_mode::ProcessWait;
use std::sync::Arc;

pub(super) async fn observe(
    manager: &UnifiedExecProcessManager,
    context: &UnifiedExecContext,
    request: &WriteStdinRequest<'_>,
) -> Option<ProcessWait> {
    let process = request.terminal.process()?;
    let target_call = manager
        .process_store
        .lock()
        .await
        .processes
        .get(&request.terminal.process_id)
        .filter(|entry| {
            Arc::ptr_eq(&entry.process, process)
                && entry.session.ptr_eq(&Arc::downgrade(&context.session))
        })?
        .call_id
        .clone();
    let service = &context.session.services.code_mode_service;
    let target = service.transcript_owner(&target_call)?;
    service
        .begin_process_wait(
            &context.call_id,
            target,
            Arc::clone(&context.step_context.turn),
        )
        .await
}
