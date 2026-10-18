use crate::ClearableField;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use codex_protocol::ContextRole;
use codex_protocol::ThreadId;
use codex_state::StateRuntime;

#[expect(
    clippy::ref_option,
    reason = "ClearableField is the canonical unchanged/clear/set patch; borrowing preserves that shared metadata representation"
)]
pub(super) async fn apply_context_role(
    state: &StateRuntime,
    thread_id: ThreadId,
    change: &ClearableField<ContextRole>,
) -> ThreadStoreResult<()> {
    if let Some(role) = change
        && !state
            .set_thread_context_role(thread_id, role.as_ref())
            .await
            .map_err(|err| ThreadStoreError::Internal {
                message: format!("failed to update context role for {thread_id}: {err}"),
            })?
    {
        return Err(ThreadStoreError::Internal {
            message: format!("thread metadata unavailable before context role update: {thread_id}"),
        });
    }
    Ok(())
}
