//! Borrows subscription-only `OpenAI` access without rebinding the conversation.

use crate::agent::child_config::switch_agent_provider;
use crate::config::Config;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::thread_manager::accounts::AccountInheritance;
use crate::thread_manager::accounts::account_auth_for_provider;
use codex_model_provider::SharedModelProvider;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_model_provider_info::SearchRoute;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use std::sync::Arc;
use std::sync::Weak;
use std::time::Duration;

pub struct ChatgptDelegate(Weak<Session>);

pub(crate) struct BorrowedChatgpt {
    pub(crate) config: Config,
    pub(crate) captured: crate::CapturedProvider,
    pub(crate) manager: SharedModelsManager,
    pub(crate) account_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ChatgptDelegateError {
    #[error("Web search for {} conversations needs an active ChatGPT login; choose one with /account.", conversation_name(*.0))]
    NoChatgptLogin(ModelProviderAdapter),
    #[error("Web search for {} conversations needs the configured openai provider.", conversation_name(*.0))]
    MissingOpenaiProvider(ModelProviderAdapter),
    #[error("Web search could not load the ChatGPT model catalog: {0}")]
    CatalogUnavailable(String),
    #[error("Web search conversation is no longer available.")]
    ConversationUnavailable,
}

fn conversation_name(adapter: ModelProviderAdapter) -> &'static str {
    match adapter {
        ModelProviderAdapter::OpenaiCompatible => "OpenAI",
        ModelProviderAdapter::ClaudeSubscription => "Claude",
        ModelProviderAdapter::OpenRouter => "OpenRouter",
    }
}

impl ChatgptDelegate {
    pub(crate) fn install(session: &Arc<Session>, config: &Config) {
        if config.model_provider.adapter.borrows_chatgpt() {
            session
                .services
                .thread_extension_data
                .insert(Self(Arc::downgrade(session)));
        }
    }

    pub async fn resolve(&self) -> Result<(SharedModelProvider, String), ChatgptDelegateError> {
        let session = self
            .0
            .upgrade()
            .ok_or(ChatgptDelegateError::ConversationUnavailable)?;
        let config = session.get_config().await;
        let BorrowedChatgpt {
            config: candidate,
            captured,
            manager,
            ..
        } = Box::pin(borrow_chatgpt(&session, &config)).await?;
        let model = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
            manager
                .list_models(
                    RefreshStrategy::OnlineIfUncached,
                    candidate.http_client_factory(),
                )
                .await
                .into_iter()
                .find(|model| model.is_default)
                .map(|model| model.model)
                .ok_or_else(|| ChatgptDelegateError::CatalogUnavailable("no default model".into()))
        })
        .await
        .map_err(|_| ChatgptDelegateError::CatalogUnavailable("request timed out".into()))??;
        Ok((Arc::clone(captured.provider()), model))
    }
}

/// Keeps the catalog and authorization on one captured account across a concurrent /account switch.
pub(crate) async fn borrow_chatgpt(
    session: &Session,
    config: &Config,
) -> Result<BorrowedChatgpt, ChatgptDelegateError> {
    if !config.model_providers.contains_key(OPENAI_PROVIDER_ID) {
        return Err(ChatgptDelegateError::MissingOpenaiProvider(
            config.model_provider.adapter,
        ));
    }
    let mut candidate = config.clone();
    switch_agent_provider(&mut candidate, OPENAI_PROVIDER_ID);
    if !uses_chatgpt_login(&candidate.model_provider) {
        return Err(ChatgptDelegateError::NoChatgptLogin(
            config.model_provider.adapter,
        ));
    }
    let auth = account_auth_for_provider(
        &candidate,
        &session.services.local_agent_runtime.account_selection,
        &session.services.auth_manager,
        /*persisted*/ None,
        &AccountInheritance::OtherProvider,
    )
    .await
    .map_err(|_| ChatgptDelegateError::NoChatgptLogin(config.model_provider.adapter))?
    .ok_or(ChatgptDelegateError::NoChatgptLogin(
        config.model_provider.adapter,
    ))?;
    if !auth
        .auth_cached()
        .is_some_and(|auth| auth.is_chatgpt_auth())
    {
        return Err(ChatgptDelegateError::NoChatgptLogin(
            config.model_provider.adapter,
        ));
    }
    let account_id = auth.selected_account_id();
    let captured = crate::CapturedProvider::capture_responses(create_model_provider(
        candidate.model_provider.clone(),
        Some(auth),
    ))
    .map_err(|error| ChatgptDelegateError::CatalogUnavailable(error.to_string()))?;
    let manager = captured
        .models_manager(&candidate)
        .map_err(|error| ChatgptDelegateError::CatalogUnavailable(error.to_string()))?;
    Ok(BorrowedChatgpt {
        config: candidate,
        captured,
        manager,
        account_id,
    })
}

pub(crate) fn uses_chatgpt_login(provider: &ModelProviderInfo) -> bool {
    provider.requires_openai_auth
        && provider.adapter == ModelProviderAdapter::OpenaiCompatible
        && provider.env_key.is_none()
        && provider.experimental_bearer_token.is_none()
        && provider.auth.is_none()
}

/// Claude and `OpenRouter` conversations search through a borrowed `ChatGPT` login, so the hosted-search
/// capability and its rollout flag do not gate them.
pub(crate) fn delegates_web_search(turn: &TurnContext) -> bool {
    matches!(
        SearchRoute::classify(
            &turn.config.model_provider,
            &turn.config.model_providers,
            turn.config.web_search_mode.value(),
        ),
        SearchRoute::ChatgptDelegate
    )
}
