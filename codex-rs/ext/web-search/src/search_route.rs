use codex_core::chatgpt_delegate::ChatgptDelegate;
use codex_extension_api::ExtensionData;
use codex_extension_api::FunctionCallError;
use codex_login::AuthManager;
use codex_model_provider::SharedModelProvider;
use codex_model_provider::create_model_provider;
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
        auth_manager: &Arc<AuthManager>,
    ) -> Option<Self> {
        match route {
            SearchRoute::Unavailable => None,
            SearchRoute::Own { provider } => Some(Self::Own(create_model_provider(
                provider.as_ref().clone(),
                Some(
                    thread_store
                        .get::<Arc<AuthManager>>()
                        .as_deref()
                        .cloned()
                        .unwrap_or_else(|| auth_manager.clone()),
                ),
            ))),
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
