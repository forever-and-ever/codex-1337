use codex_core::chatgpt_delegate::ChatgptDelegate;
use codex_extension_api::ExtensionData;
use codex_extension_api::FunctionCallError;
use codex_model_provider::SharedModelProvider;
use codex_model_provider_info::SearchRoute;
use std::sync::Arc;

pub(crate) enum SearchExecutor {
    Own(SharedModelProvider),
    ChatgptDelegate(Arc<ChatgptDelegate>),
}

impl SearchExecutor {
    pub(crate) fn from_route(
        route: &SearchRoute,
        thread_store: &ExtensionData,
        step_store: &ExtensionData,
    ) -> Option<Self> {
        match route {
            SearchRoute::Unavailable => None,
            // Core captures the selected provider under step admission before contributors run.
            // Never recreate it from mutable thread credentials or an installation fallback.
            SearchRoute::Own { .. } => step_store
                .get::<SharedModelProvider>()
                .map(|provider| Self::Own(Arc::clone(provider.as_ref()))),
            SearchRoute::ChatgptDelegate => thread_store
                .get::<ChatgptDelegate>()
                .map(Self::ChatgptDelegate),
        }
    }

    pub(crate) async fn resolve(
        &self,
        model: &str,
    ) -> Result<(SharedModelProvider, String), FunctionCallError> {
        match self {
            Self::Own(provider) => Ok((provider.clone(), model.to_owned())),
            Self::ChatgptDelegate(delegate) => Box::pin(delegate.resolve())
                .await
                .map_err(|error| FunctionCallError::RespondToModel(error.to_string())),
        }
    }
}
