//! Accepted mail distinguishes canonical commitment from rebuildable metadata projection.

use super::LiveThread;
use crate::LocalThreadStore;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use crate::UpdateThreadMetadataParams;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::InterAgentCommunication;
use codex_rollout::ResponseItemEnvelope;
use codex_rollout::RolloutItem;
use codex_rollout::persisted_rollout_items;
use std::collections::HashMap;
use std::collections::HashSet;
use std::future::Future;

pub enum AgentMailCommit {
    Complete,
    MetadataPending(ThreadStoreError),
}

pub trait AgentMailPersistence {
    /// Searches retained raw canonical history, including items removed from model context.
    fn find_accepted_agent_mail(
        &self,
        ids: &[ResponseItemId],
    ) -> impl Future<Output = ThreadStoreResult<HashMap<ResponseItemId, ResponseItemEnvelope>>> + Send;

    /// Err means canonical completion is uncertain, not that no bytes were written.
    fn append_accepted_agent_mail(
        &self,
        envelope: &ResponseItemEnvelope,
    ) -> impl Future<Output = ThreadStoreResult<AgentMailCommit>> + Send;
}

impl AgentMailPersistence for LiveThread {
    async fn find_accepted_agent_mail(
        &self,
        ids: &[ResponseItemId],
    ) -> ThreadStoreResult<HashMap<ResponseItemId, ResponseItemEnvelope>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        if let Some(store) = self
            .thread_store
            .as_any()
            .downcast_ref::<LocalThreadStore>()
        {
            return store.find_accepted_agent_mail(self.thread_id, ids).await;
        }
        // Nonlocal full-history stores retain their existing canonical history contract.
        // A store that cannot supply that history fails closed, not via volatile context.
        let history = self.load_history(true).await?;
        let ids = ids.iter().cloned().collect();
        let mut found = HashMap::new();
        for item in history.items {
            if let RolloutItem::ResponseItem(envelope) = item {
                retain_accepted_agent_mail(&mut found, &ids, envelope)?;
            }
        }
        Ok(found)
    }

    async fn append_accepted_agent_mail(
        &self,
        envelope: &ResponseItemEnvelope,
    ) -> ThreadStoreResult<AgentMailCommit> {
        let raw_items = [
            RolloutItem::InterAgentCommunicationMetadata {
                trigger_turn: false,
            },
            RolloutItem::ResponseItem(envelope.clone()),
        ];
        let items = self
            .persist_appended_items(&raw_items)
            .await?
            .unwrap_or_else(|| persisted_rollout_items(&raw_items, self.history_mode));
        Ok(match self.sync_appended_metadata(&items).await {
            Ok(()) => AgentMailCommit::Complete,
            Err(error) => AgentMailCommit::MetadataPending(error),
        })
    }
}

pub fn validate_accepted_agent_mail(
    envelope: &ResponseItemEnvelope,
    communication: &InterAgentCommunication,
) -> ThreadStoreResult<()> {
    if communication.trigger_turn
        || communication.id.is_none()
        || envelope.id() != communication.id.as_ref()
        || !same_agent_mail(&envelope.item, &communication.to_model_input_item())
    {
        return Err(ThreadStoreError::Conflict {
            message: format!(
                "canonical agent-mail identity {:?} has conflicting attribution, content or mode",
                communication.id
            ),
        });
    }
    Ok(())
}

pub const ACCEPTED_AGENT_MAIL_ID_KIND: &str = "amsg_mail";

pub fn is_accepted_agent_mail_id(id: &ResponseItemId) -> bool {
    id.strip_prefix(ACCEPTED_AGENT_MAIL_ID_KIND)
        .is_some_and(|suffix| suffix.starts_with('_'))
}

fn same_agent_mail(left: &ResponseItem, right: &ResponseItem) -> bool {
    matches!((left, right), (
        ResponseItem::AgentMessage { author: left_author, recipient: left_recipient, content: left_content, .. },
        ResponseItem::AgentMessage { author: right_author, recipient: right_recipient, content: right_content, .. },
    ) if left_author == right_author && left_recipient == right_recipient && left_content == right_content)
}

pub(crate) fn retain_accepted_agent_mail(
    found: &mut HashMap<ResponseItemId, ResponseItemEnvelope>,
    ids: &HashSet<ResponseItemId>,
    envelope: ResponseItemEnvelope,
) -> ThreadStoreResult<()> {
    let Some(id) = envelope.id().filter(|id| ids.contains(*id)).cloned() else {
        return Ok(());
    };
    if let Some(previous) = found.get(&id) {
        if !same_agent_mail(&previous.item, &envelope.item) {
            return Err(ThreadStoreError::Conflict {
                message: format!(
                    "canonical agent-mail identity {id} has conflicting attribution or content"
                ),
            });
        }
    } else {
        found.insert(id, envelope);
    }
    Ok(())
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
