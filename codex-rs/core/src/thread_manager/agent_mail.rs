//! Headless recovery of accepted mail through the recipient's original family authority.

use super::ThreadManager;
use codex_protocol::ThreadId;
use codex_protocol::error::Result;

impl ThreadManager {
    pub async fn recover_accepted_agent_mail(
        &self,
        root: ThreadId,
        recipient: ThreadId,
    ) -> Result<()> {
        let thread = self.state.get_thread(root).await?;
        Box::pin(
            thread
                .session
                .services
                .local_agent_runtime
                .recover_accepted_agent_mail(root, recipient),
        )
        .await
    }
}
