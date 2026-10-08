use super::CodexThread;
use crate::WarmingLore;

impl CodexThread {
    /// Lore for this thread's next request; none when its provider gives none.
    pub async fn prompt_cache_lore(&self) -> Option<WarmingLore> {
        self.session.prompt_cache_lore().await
    }
}
