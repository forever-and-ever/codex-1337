use crate::ModelProviderAdapter;
use crate::ModelProviderInfo;
use crate::OPENAI_PROVIDER_ID;
use codex_protocol::config_types::WebSearchMode;
use std::collections::HashMap;

#[derive(Clone)]
pub enum SearchRoute {
    Unavailable,
    Own { provider: Box<ModelProviderInfo> },
    ChatgptDelegate,
}

impl SearchRoute {
    pub fn classify(
        provider: &ModelProviderInfo,
        providers: &HashMap<String, ModelProviderInfo>,
        mode: WebSearchMode,
    ) -> Self {
        if mode == WebSearchMode::Disabled {
            Self::Unavailable
        } else if provider.is_openai()
            || provider.uses_openai_actor_authorization()
            || provider.supports_standalone_web_search
        {
            Self::Own {
                provider: Box::new(provider.clone()),
            }
        } else if provider.adapter == ModelProviderAdapter::ClaudeSubscription
            && providers.contains_key(OPENAI_PROVIDER_ID)
        {
            Self::ChatgptDelegate
        } else {
            Self::Unavailable
        }
    }
}
