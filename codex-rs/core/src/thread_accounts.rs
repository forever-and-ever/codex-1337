use super::*;
use crate::agent::control::ProviderAccountSelection;
use crate::agent::control::ProviderAccountSelections;
use codex_model_provider_info::ModelProviderAdapter;

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
        let inherited = if let Some(parent) = parent {
            // A resumed child can outlive its loaded parent. Its persisted
            // binding remains authoritative when the parent has been evicted.
            self.threads
                .read()
                .await
                .get(&parent)
                .map(|thread| thread.session.services.auth_manager.clone())
        } else {
            None
        };
        if config.model_provider.adapter == ModelProviderAdapter::ClaudeSubscription
            && config.model_provider.requires_openai_auth
        {
            return Err(CodexErr::InvalidRequest(
                "Claude subscription providers must disable OpenAI authentication".into(),
            ));
        }
        if !config.model_provider.requires_openai_auth {
            if config.model_provider.adapter == ModelProviderAdapter::ClaudeSubscription {
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
                if selected.is_none()
                    && let Some(id) =
                        persisted.or_else(|| config.wrangler.default_claude_account.clone())
                {
                    agent_control.runtime().account_selection.select(
                        &config.model_provider_id,
                        ProviderAccountSelection::ClaudeSubscription { id },
                    );
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
            inherited.as_ref(),
        )
        .await
    }
}

pub(crate) async fn account_auth_for_provider(
    config: &Config,
    selections: &ProviderAccountSelections,
    auth: &Arc<AuthManager>,
    persisted: Option<String>,
    inherited: Option<&Arc<AuthManager>>,
) -> CodexResult<Option<Arc<AuthManager>>> {
    if !config.model_provider.requires_openai_auth {
        return Ok(None);
    }
    let source = inherited.unwrap_or(auth);
    if persisted.is_none()
        && (source.has_external_auth()
            || source
                .auth_cached()
                .is_some_and(|auth| !auth.is_chatgpt_auth()))
    {
        return Ok(None);
    }
    let id = selections
        .get(&config.model_provider_id)
        .and_then(|selection| match selection.as_ref() {
            ProviderAccountSelection::Chatgpt { id } => Some(id.clone()),
            ProviderAccountSelection::ClaudeSubscription { .. } => None,
        })
        .or(persisted)
        .or_else(|| inherited.and_then(|auth| auth.selected_account_id()))
        .or_else(|| config.wrangler.default_account.clone())
        .or_else(|| source.selected_account_id());
    match id {
        Some(id) => source.for_account(&id).await.map(Some).map_err(|error| {
            CodexErr::InvalidRequest(format!("Cannot load conversation account: {error}"))
        }),
        None => Ok(None),
    }
}
