//! TELOS(warpaint): explicit nullable preferences bypass rollout-derived metadata.

use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use codex_protocol::ThreadId;
use codex_state::StateRuntime;

pub(super) async fn apply_theme(
    state: &StateRuntime,
    thread: ThreadId,
    theme: Option<&str>,
) -> ThreadStoreResult<()> {
    let updated = state
        .set_thread_theme(thread, theme)
        .await
        .map_err(|error| ThreadStoreError::Internal {
            message: format!("failed to save theme for {thread}: {error}"),
        })?;
    if !updated {
        return Err(ThreadStoreError::Internal {
            message: format!("thread metadata unavailable before theme update: {thread}"),
        });
    }
    Ok(())
}
