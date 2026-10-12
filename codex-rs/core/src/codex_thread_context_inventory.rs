use super::CodexThread;
use codex_protocol::ContextInventory;
impl CodexThread {
    pub async fn context_inventory(&self) -> Result<ContextInventory, String> {
        self.session.context_inventory().await
    }
}
