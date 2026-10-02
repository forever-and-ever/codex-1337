//! Converts agent messages into attributed input while preserving their wake mode.

use super::LocalAgentControl;
use crate::agent::types::AgentMessage;
use crate::agent::types::MessageDeliveryMode;
use crate::context::ContextualUserFragment;
use crate::context::InterAgentMessage;
use crate::context::InterAgentMessageType;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::InterAgentCommunication;

impl LocalAgentControl {
    pub(super) async fn validate_message_provider(
        &self,
        caller: ThreadId,
        recipient_provider: &str,
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
        }
        Ok(())
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
                    MessageDeliveryMode::QueueOnly => InterAgentMessageType::Message,
                    MessageDeliveryMode::TriggerTurn => InterAgentMessageType::NewTask,
                };
                let content = InterAgentMessage::new(
                    message_type,
                    recipient.clone(),
                    author.clone(),
                    message,
                )
                .render();
                InterAgentCommunication::new(author, recipient, Vec::new(), content, trigger_turn)
            }
        }
    }
}
