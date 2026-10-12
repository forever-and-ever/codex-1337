//! Accepted mail distinguishes canonical commitment from rebuildable metadata projection.

use super::LiveThread;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use crate::UpdateThreadMetadataParams;
use codex_protocol::models::ResponseItemEnvelope;
use codex_rollout::RolloutItem;
use std::future::Future;

pub enum AgentMailCommit {
    Complete,
    MetadataPending(ThreadStoreError),
}

pub trait AgentMailPersistence {
    /// Err means canonical completion is uncertain, not that no bytes were written.
    fn append_accepted_agent_mail(
        &self,
        envelope: &ResponseItemEnvelope,
    ) -> impl Future<Output = ThreadStoreResult<AgentMailCommit>> + Send;
}

impl AgentMailPersistence for LiveThread {
    async fn append_accepted_agent_mail(
        &self,
        envelope: &ResponseItemEnvelope,
    ) -> ThreadStoreResult<AgentMailCommit> {
        let items = self
            .persist_appended_items(&[
                RolloutItem::InterAgentCommunicationMetadata {
                    trigger_turn: false,
                },
                RolloutItem::ResponseItem(envelope.clone()),
            ])
            .await?;
        Ok(match self.sync_appended_metadata(&items).await {
            Ok(()) => AgentMailCommit::Complete,
            Err(error) => AgentMailCommit::MetadataPending(error),
        })
    }
}

impl LiveThread {
    pub(super) async fn sync_appended_metadata(
        &self,
        items: &[RolloutItem],
    ) -> ThreadStoreResult<()> {
        let update = self
            .metadata_sync
            .lock()
            .await
            .observe_appended_items(items);
        if let Some(update) = update {
            self.thread_store
                .record_thread_metadata(UpdateThreadMetadataParams {
                    thread_id: self.thread_id,
                    patch: update.patch.clone(),
                    include_archived: true,
                })
                .await?;
            self.metadata_sync
                .lock()
                .await
                .mark_pending_update_applied(&update);
        }
        Ok(())
    }
}
