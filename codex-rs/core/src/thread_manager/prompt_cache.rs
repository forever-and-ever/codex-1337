use super::ThreadManager;
use crate::PromptCacheManager;
use crate::PromptCacheObserver;
use std::sync::Arc;

impl PromptCacheManager for ThreadManager {
    fn with_prompt_cache_observer(self, observer: Arc<dyn PromptCacheObserver>) -> Self {
        assert!(
            self.state.prompt_cache_observer.set(observer).is_ok(),
            "one prompt-cache observer per manager"
        );
        self
    }
}
