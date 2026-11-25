use std::collections::HashSet;
use std::path::Path;

use codex_protocol::ThreadId;
use codex_rollout::QueuedInputReceipt;
use codex_rollout::StateDbHandle;
use codex_rollout::read_queued_input_receipts;

use super::LocalThreadStore;
use super::live_writer::live_writer_parts;
use crate::MAX_QUEUE_ITEMS;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

impl LocalThreadStore {
    pub(crate) async fn queued_input_receipts(
        &self,
        thread_id: ThreadId,
    ) -> ThreadStoreResult<Vec<QueuedInputReceipt>> {
        let _writer = self.live_writer_locks.lock(thread_id).await;
        let (recorder, _, _) = live_writer_parts(self, thread_id).await?;
        if recorder
            .unpersisted_session_meta()
            .await
            .map_err(queue_history_error)?
            .is_some()
        {
            return Ok(Vec::new());
        }
        read_queued_input_receipts(recorder.rollout_path())
            .await
            .map_err(queue_history_error)
    }

    /// Revert can discard the only receipt for a queue row whose cleanup failed.
    /// Settle those rows while the caller still holds the old generation's writer lock.
    pub(super) async fn clean_queued_input_before_revert(
        &self,
        state_db: &StateDbHandle,
        thread_id: ThreadId,
        path: &Path,
    ) -> ThreadStoreResult<()> {
        let queue = state_db.thread_queue();
        let pending = queue
            .list_page(thread_id, 0, MAX_QUEUE_ITEMS)
            .await
            .map_err(queue_history_error)?;
        if pending.is_empty() {
            return Ok(());
        }
        let pending: HashSet<_> = pending.into_iter().map(|record| record.id).collect();
        for receipt in read_queued_input_receipts(path)
            .await
            .map_err(queue_history_error)?
        {
            if pending.contains(&receipt.id) {
                queue
                    .delete(thread_id, &receipt.id)
                    .await
                    .map_err(queue_history_error)?;
            }
        }
        Ok(())
    }
}

fn queue_history_error(error: impl std::fmt::Display) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: format!("cannot reconcile durable queued input: {error}"),
    }
}
