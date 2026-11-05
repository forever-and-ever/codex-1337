use super::{AgentControlInit, AuthManager, Config, InitialHistory, ThreadId, ThreadManagerState};
use crate::StartCredentials;
use codex_protocol::error::Result;
use std::sync::Arc;

impl StartCredentials {
    pub(super) async fn account_for_spawn(
        &self,
        manager: &ThreadManagerState,
        config: &Config,
        history: &InitialHistory,
        parent: Option<ThreadId>,
        credentials: &Arc<AuthManager>,
        control: &AgentControlInit,
    ) -> Result<Option<Arc<AuthManager>>> {
        match self {
            Self::Inherit => {
                Box::pin(manager.account_for_spawn(config, history, parent, credentials, control))
                    .await
            }
            Self::Captured(captured) => Ok(captured.prepare_spawn(config, control)?),
        }
    }
}
