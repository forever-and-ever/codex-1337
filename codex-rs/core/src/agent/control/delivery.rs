//! Converts agent messages into attributed input while preserving their wake mode.

use super::LocalAgentControl;
use crate::agent::types::AgentMessage;
use crate::agent::types::MessageDeliveryMode;
use crate::context::ContextualUserFragment;
use crate::context::InterAgentMessage;
use crate::context::InterAgentMessageType;
use codex_model_provider_info::ModelProviderAdapter;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::InterAgentCommunication;
use tokio::sync::OwnedRwLockReadGuard;

impl LocalAgentControl {
    pub(super) async fn validate_message_provider(
        &self,
        caller: ThreadId,
        recipient_provider: &str,
        recipient_adapter: ModelProviderAdapter,
        recipient_account_id: Option<&str>,
        message: &AgentMessage,
    ) -> CodexResult<()> {
        if matches!(message, AgentMessage::Encrypted(_)) {
            let manager = self.runtime.upgrade()?;
            let sender = manager.get_thread(caller).await?;
            if sender.config_snapshot().await.model_provider_id != recipient_provider {
                return Err(CodexErr::InvalidRequest(
                    "Encrypted agent messages cannot cross providers; retry using plaintext_message".into(),
                ));
            }
            // The native Messages encoder cannot consume provider-owned ciphertext;
            // history salvage would otherwise omit the task entirely.
            if recipient_adapter.speaks_anthropic_messages() {
                return Err(CodexErr::InvalidRequest(
                    "Native Anthropic cannot read encrypted agent messages; retry using plaintext_message".into(),
                ));
            }
            if sender.auth_manager().selected_account_id().as_deref() != recipient_account_id {
                return Err(CodexErr::InvalidRequest(
                    "Encrypted agent messages cannot cross accounts; retry using plaintext_message"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) async fn ensure_message_recipient_loaded(
        &self,
        recipient: ThreadId,
        author: &AgentPath,
    ) -> CodexResult<Option<OwnedRwLockReadGuard<()>>> {
        let manager = self.runtime.upgrade()?;
        if let Ok(thread) = manager.get_thread(recipient).await {
            return self.runtime.pin_v2_residency(&thread).await;
        }
        let sender = self
            .runtime
            .registry
            .agent_id_for_path(author)
            .ok_or(CodexErr::ThreadNotFound(recipient))?;
        let config = manager.get_thread(sender).await?.session.get_config().await;
        self.ensure_v2_agent_loaded((*config).clone(), recipient, None)
            .await
    }
}

impl AgentMessage {
    pub(crate) fn into_communication(
        self,
        author: AgentPath,
        recipient: AgentPath,
        mode: MessageDeliveryMode,
    ) -> InterAgentCommunication {
        let trigger_turn = mode == MessageDeliveryMode::TriggerTurn;
        match self {
            Self::Encrypted(message) => InterAgentCommunication::new_encrypted(
                author,
                recipient,
                Vec::new(),
                message,
                trigger_turn,
            ),
            Self::Plaintext(message) => {
                let message_type = match mode {
                    MessageDeliveryMode::Ordinary => InterAgentMessageType::Message,
                    MessageDeliveryMode::TriggerTurn => InterAgentMessageType::NewTask,
                };
                let content = InterAgentMessage::new(
                    message_type,
                    recipient.clone(),
                    author.clone(),
                    message,
                )
                .render()
                .into_text()
                .unwrap_or_else(|| unreachable!("inter-agent messages render as text"));
                InterAgentCommunication::new(author, recipient, Vec::new(), content, trigger_turn)
            }
        }
    }
}
