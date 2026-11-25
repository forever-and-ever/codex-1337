use codex_rollout::QueuedInputReceipt;
use codex_rollout::RolloutItem;

use super::LiveThread;
use crate::LocalThreadStore;
use crate::ThreadStoreResult;

impl LiveThread {
    pub async fn queued_input_receipts(&self) -> ThreadStoreResult<Vec<QueuedInputReceipt>> {
        if let Some(store) = self
            .thread_store
            .as_any()
            .downcast_ref::<LocalThreadStore>()
        {
            return store.queued_input_receipts(self.thread_id).await;
        }
        let history = self.load_history(true).await?;
        Ok(history
            .items
            .into_iter()
            .filter_map(|item| match item {
                RolloutItem::ResponseItem(envelope) => envelope.metadata?.queued_input,
                _ => None,
            })
            .collect())
    }
}
