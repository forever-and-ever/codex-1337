use super::AgentControlInit;
use super::Arc;
use super::AuthManager;
use super::CodexErr;
use super::CodexResult;
use super::Config;
use super::EventMsg;
use super::InitialHistory;
use super::LocalAgentControl;
use super::RolloutItem;
use super::ThreadId;
use super::ThreadManager;
use super::ThreadManagerState;
use crate::agent::control::ProviderAccountSelection;
use crate::agent::control::ProviderAccountSelections;
use codex_model_provider_info::ModelProviderAdapter;

/// A live fork freezes committed credentials, never the family's pending retarget.
pub(crate) struct ForkAccountScope {
    provider_id: String,
    provider: codex_model_provider_info::ModelProviderInfo,
    auth: Arc<AuthManager>,
    selection: Option<ProviderAccountSelection>,
}

impl ForkAccountScope {
    /// The source's settings persistence permit must cover config and account capture.
    pub(super) async fn capture(
        source: &crate::session::session::Session,
        config: &Config,
    ) -> CodexResult<Self> {
        let auth = &source.services.auth_manager;
        let selection = source
            .services
            .model_client
            .provider_account_selection()
            .or_else(|| {
                config
                    .model_provider
                    .requires_openai_auth
                    .then(|| auth.selected_account_id())
                    .flatten()
                    .map(|id| ProviderAccountSelection::Chatgpt { id })
            });
        let auth = match &selection {
            Some(ProviderAccountSelection::Chatgpt { id }) => {
                auth.for_account(id).await.map_err(|error| {
                    CodexErr::InvalidRequest(format!("Cannot capture fork account: {error}"))
                })?
            }
            Some(
                ProviderAccountSelection::ClaudeSubscription { .. }
                | ProviderAccountSelection::OpenRouter { .. },
            )
            | None => Arc::clone(auth),
        };
        Ok(Self {
            provider_id: config.model_provider_id.clone(),
            provider: config.model_provider.clone(),
            auth,
            selection,
        })
    }

    pub(super) fn restore(
        &self,
        config: &Config,
        control: &LocalAgentControl,
    ) -> CodexResult<Arc<AuthManager>> {
        if self.provider_id != config.model_provider_id || self.provider != config.model_provider {
            return Err(CodexErr::InvalidRequest(
                "fork provider changed after its account scope was captured".into(),
            ));
        }
        if let Some(selection) = &self.selection {
            control
                .runtime
                .account_selection
                .select(&self.provider_id, selection.clone());
        }
        Ok(Arc::clone(&self.auth))
    }
}

pub(crate) enum AccountInheritance {
    None,
    SameProvider(Arc<AuthManager>),
    OtherProvider,
}

impl ThreadManager {
    /// Resolves a fresh root conversation's account without creating a conversation.
    pub async fn auth_for_new_thread(&self, config: &Config) -> CodexResult<Arc<AuthManager>> {
        Ok(account_auth_for_provider(
            config,
            &ProviderAccountSelections::default(),
            &self.state.auth_manager,
            /*persisted*/ None,
            &AccountInheritance::None,
        )
        .await?
        .unwrap_or_else(|| self.state.auth_manager.clone()))
    }
}

impl ThreadManagerState {
    pub(super) async fn account_for_spawn(
        &self,
        config: &Config,
        history: &InitialHistory,
        parent: Option<ThreadId>,
        auth: &Arc<AuthManager>,
        agent_control: &AgentControlInit,
    ) -> CodexResult<Option<Arc<AuthManager>>> {
        let owner = match history {
            InitialHistory::Resumed(history) => Some(history.conversation_id),
            InitialHistory::Forked(_) => history.forked_from_id(),
            InitialHistory::New | InitialHistory::Cleared => None,
        };
        let persisted = history
            .get_rollout_items()
            .iter()
            .rev()
            .find_map(|item| match item {
                RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event))
                    if owner.is_some()
                        && event.thread_id == owner
                        && event.thread_settings.model_provider_id == config.model_provider_id =>
                {
                    event.thread_settings.account_id.clone()
                }
                _ => None,
            });
        if config.model_provider.adapter == ModelProviderAdapter::OpenRouter {
            let selected = agent_control
                .runtime()
                .account_selection
                .get(&config.model_provider_id)
                .is_some_and(|selection| {
                    matches!(
                        selection.as_ref(),
                        ProviderAccountSelection::OpenRouter { .. }
                    )
                });
            if !selected {
                let key = match persisted {
                    Some(key) => Some(key.parse().map_err(CodexErr::InvalidRequest)?),
                    None => codex_config::read_openrouter_config(config.codex_home.as_path())
                        .await?
                        .default_key()
                        .cloned(),
                };
                if let Some(key) = key {
                    agent_control.runtime().account_selection.select(
                        &config.model_provider_id,
                        ProviderAccountSelection::OpenRouter { key },
                    );
                }
            }
            return Ok(None);
        }
        let inherited = if let Some(parent) = parent {
            // A resumed child can outlive its loaded parent. Its persisted
            // binding remains authoritative when the parent has been evicted.
            self.threads
                .read()
                .await
                .get(&parent)
                .map_or(AccountInheritance::None, |thread| {
                    if thread.session.provider_id == config.model_provider_id {
                        AccountInheritance::SameProvider(
                            thread.session.services.auth_manager.clone(),
                        )
                    } else {
                        // Incidental credentials carried by another provider are not a binding.
                        AccountInheritance::OtherProvider
                    }
                })
        } else {
            AccountInheritance::None
        };
        if config.model_provider.adapter.speaks_anthropic_messages()
            && config.model_provider.requires_openai_auth
        {
            return Err(CodexErr::InvalidRequest(
                "Claude subscription providers must disable OpenAI authentication".into(),
            ));
        }
        if !config.model_provider.requires_openai_auth {
            if config.model_provider.adapter.speaks_anthropic_messages() {
                let selected = agent_control
                    .runtime()
                    .account_selection
                    .get(&config.model_provider_id)
                    .filter(|selection| {
                        matches!(
                            selection.as_ref(),
                            ProviderAccountSelection::ClaudeSubscription { .. }
                        )
                    });
                if selected.is_none() {
                    let id = match persisted {
                        Some(id) => Some(id),
                        None if matches!(inherited, AccountInheritance::OtherProvider) => {
                            codex_config::read_account_preferences(config.codex_home.as_path())
                                .await?
                                .unwrap_or_else(|| config.wrangler.clone())
                                .default_claude_account
                        }
                        None => config.wrangler.default_claude_account.clone(),
                    };
                    if let Some(id) = id {
                        agent_control.runtime().account_selection.select(
                            &config.model_provider_id,
                            ProviderAccountSelection::ClaudeSubscription { id },
                        );
                    }
                }
            }
            // Never scope a provider-backed thread to an ambient ChatGPT login.
            return Ok(None);
        }
        account_auth_for_provider(
            config,
            &agent_control.runtime().account_selection,
            auth,
            persisted,
            &inherited,
        )
        .await
    }
}

pub(crate) async fn account_auth_for_provider(
    config: &Config,
    selections: &ProviderAccountSelections,
    auth: &Arc<AuthManager>,
    persisted: Option<String>,
    inheritance: &AccountInheritance,
) -> CodexResult<Option<Arc<AuthManager>>> {
    if !config.model_provider.requires_openai_auth {
        return Ok(None);
    }
    let inherited = match inheritance {
        AccountInheritance::SameProvider(auth) => Some(auth),
        AccountInheritance::None | AccountInheritance::OtherProvider => None,
    };
    let source = inherited.unwrap_or(auth);
    if persisted.is_none()
        && (source.has_external_auth()
            || source
                .auth_cached()
                .is_some_and(|auth| !auth.is_chatgpt_auth()))
    {
        return Ok(None);
    }
    let binding = selections
        .get(&config.model_provider_id)
        .and_then(|selection| match selection.as_ref() {
            ProviderAccountSelection::Chatgpt { id } => Some(id.clone()),
            ProviderAccountSelection::ClaudeSubscription { .. }
            | ProviderAccountSelection::OpenRouter { .. } => None,
        })
        .or(persisted)
        .or_else(|| inherited.and_then(|auth| auth.selected_account_id()));
    // Unbound conversations take the active login, read from storage so that neither a startup
    // snapshot nor another provider's incidental manager overrides a newer `/account` choice.
    let id = match binding {
        Some(id) => Some(id),
        None => source.active_account_id()?,
    };
    match id {
        Some(id) => source.for_account(&id).await.map(Some).map_err(|error| {
            CodexErr::InvalidRequest(format!("Cannot load conversation account: {error}"))
        }),
        None => Ok(None),
    }
}
