use super::ClientResponsePayload;
use super::ConnectionRequestId;
use super::JSONRPCErrorError;
use super::Result;
use super::TurnRequestProcessor;
use super::claude_account_controller::ClaudeAccountController;
use super::internal_error;
use super::invalid_request;
use codex_app_server_protocol::AccountNicknameSetParams;
use codex_app_server_protocol::AccountNicknameSetResponse;
use codex_app_server_protocol::AccountProvider;
use codex_app_server_protocol::SavedAccount;
use codex_app_server_protocol::ThreadAccountListParams;
use codex_app_server_protocol::ThreadAccountListResponse;
use codex_app_server_protocol::ThreadAccountSelectParams;
use codex_app_server_protocol::ThreadAccountSelectResponse;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;

impl TurnRequestProcessor {
    pub(crate) async fn thread_account_list(
        &self,
        params: ThreadAccountListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        let config = self
            .config_manager
            .load_non_project_config()
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        let accounts = self
            .thread_manager
            .auth_manager()
            .saved_accounts()
            .map_err(|error| internal_error(error.to_string()))?
            .into_iter()
            .map(|account| SavedAccount {
                nickname: config
                    .wrangler
                    .account_nicknames
                    .get(AccountProvider::Openai.config_id())
                    .and_then(|names| names.get(&account.id))
                    .cloned(),
                alias: config
                    .wrangler
                    .account_aliases
                    .iter()
                    .find_map(|(alias, id)| (id == &account.id).then(|| alias.clone())),
                id: account.id,
                email: account.email,
                plan: account.plan,
            })
            .collect();
        let (claude_accounts, claude_management_error) =
            match ClaudeAccountController::new(&config).list(&config) {
                Ok(accounts) => (accounts, None),
                Err(error) => (Vec::new(), Some(error.to_string())),
            };
        let thread_provider = thread.config_snapshot().await;
        let is_claude = thread_provider
            .model_provider_adapter
            .speaks_anthropic_messages();
        let is_chatgpt = thread_provider.requires_openai_auth;
        let is_openrouter = matches!(
            thread_provider.model_provider_adapter,
            codex_model_provider_info::ModelProviderAdapter::OpenRouter
        );
        let openrouter = codex_config::read_openrouter_config(&config.codex_home)
            .await
            .map_err(|_| internal_error("Cannot read OpenRouter preferences"))?;
        Ok(Some(
            ThreadAccountListResponse {
                conversation_provider: if is_openrouter {
                    Some(AccountProvider::OpenRouter)
                } else if is_claude {
                    Some(AccountProvider::ClaudeSubscription)
                } else if is_chatgpt {
                    Some(AccountProvider::Openai)
                } else {
                    None
                },
                accounts,
                selected_account_id: if is_chatgpt {
                    thread.auth_manager().selected_account_id()
                } else {
                    self.thread_manager
                        .auth_manager()
                        .active_account_id()
                        .map_err(|error| internal_error(error.to_string()))?
                },
                pending_account_id: is_chatgpt.then(|| thread.pending_account_id()).flatten(),
                claude_accounts,
                selected_claude_account_id: if is_claude {
                    thread.committed_provider_account_id()
                } else {
                    config.wrangler.default_claude_account.clone()
                },
                pending_claude_account_id: is_claude.then(|| thread.pending_account_id()).flatten(),
                claude_management_error,
                openrouter_keys: self.openrouter_key_statuses(&config, None).await?,
                selected_openrouter_key: if is_openrouter {
                    thread
                        .committed_provider_account_id()
                        .map(|name| name.parse().map_err(internal_error))
                        .transpose()?
                } else {
                    openrouter.default_key().cloned()
                },
                pending_openrouter_key: if is_openrouter {
                    thread
                        .pending_account_id()
                        .map(|name| name.parse().map_err(internal_error))
                        .transpose()?
                } else {
                    None
                },
            }
            .into(),
        ))
    }

    pub(crate) async fn thread_account_select(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadAccountSelectParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        self.ensure_direct_input_allowed(request_id, thread.as_ref())
            .await?;
        let config = self
            .config_manager
            .load_non_project_config()
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        let thread_provider = thread.config_snapshot().await;
        let is_claude_thread = thread_provider
            .model_provider_adapter
            .speaks_anthropic_messages();
        let is_chatgpt_thread = thread_provider.requires_openai_auth;
        // Preserve the pre-Claude API default without guessing billing from an ID string.
        let provider = params.provider.unwrap_or(AccountProvider::Openai);
        if provider == AccountProvider::OpenRouter {
            return Box::pin(self.openrouter_key_select(&config, &thread, params.account_id)).await;
        }
        if provider == AccountProvider::ClaudeSubscription {
            let _lock = codex_config::lock_account_preferences(config.codex_home.as_path())
                .await
                .map_err(|error| internal_error(error.to_string()))?;
            let controller = ClaudeAccountController::new(&config);
            controller
                .validate_selection(&params.account_id)
                .map_err(|error| invalid_request(error.to_string()))?;
            if is_claude_thread {
                Box::pin(thread.select_claude_account(&params.account_id))
                    .await
                    .map_err(|error| invalid_request(error.to_string()))?;
            }
            let default_saved = ConfigEditsBuilder::for_config(&config)
                .with_edits([ConfigEdit::SetPath {
                    segments: vec!["wrangler".into(), "default_claude_account".into()],
                    value: toml_edit::value(&params.account_id),
                }])
                .apply()
                .await
                .map_err(|error| tracing::warn!(%error, "could not save default Claude account"))
                .is_ok();
            return Ok(Some(
                ThreadAccountSelectResponse {
                    account_id: params.account_id,
                    provider,
                    conversation_changed: is_claude_thread,
                    pending: is_claude_thread && thread.pending_account_id().is_some(),
                    default_saved,
                }
                .into(),
            ));
        }
        let id = config
            .wrangler
            .account_aliases
            .get(&params.account_id)
            .cloned()
            .unwrap_or(params.account_id);
        if !self
            .thread_manager
            .auth_manager()
            .saved_accounts()
            .map_err(|error| internal_error(error.to_string()))?
            .iter()
            .any(|account| account.id == id)
        {
            return Err(invalid_request("ChatGPT account is not saved"));
        }
        if is_chatgpt_thread {
            Box::pin(thread.select_account(&id))
                .await
                .map_err(|error| invalid_request(error.to_string()))?;
        }
        // The active login, not configuration, records the choice for new conversations.
        let default_saved = self
            .thread_manager
            .auth_manager()
            .activate_account(&id)
            .await
            .map_err(|error| tracing::warn!(%error, "could not activate account"))
            .is_ok();
        Ok(Some(
            ThreadAccountSelectResponse {
                account_id: id,
                provider,
                conversation_changed: is_chatgpt_thread,
                pending: is_chatgpt_thread && thread.pending_account_id().is_some(),
                default_saved,
            }
            .into(),
        ))
    }
}

impl TurnRequestProcessor {
    pub(crate) async fn account_nickname_set(
        &self,
        params: AccountNicknameSetParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let nickname = codex_config::account_nickname(&params.nickname)
            .map_err(|error| invalid_request(error.to_string()))?;
        let initial_config = self
            .config_manager
            .load_non_project_config()
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        let _lock = codex_config::lock_account_preferences(initial_config.codex_home.as_path())
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        let config = self
            .config_manager
            .load_non_project_config()
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        match params.provider {
            AccountProvider::OpenRouter => {
                return Err(invalid_request(
                    "OpenRouter keys are named by their key name",
                ));
            }
            AccountProvider::Openai => {
                if !self
                    .thread_manager
                    .auth_manager()
                    .saved_accounts()
                    .map_err(|error| internal_error(error.to_string()))?
                    .iter()
                    .any(|account| account.id == params.account_id)
                {
                    return Err(invalid_request("ChatGPT account is not saved"));
                }
                if !nickname.is_empty()
                    && config.wrangler.account_aliases.iter().any(|(alias, id)| {
                        id != &params.account_id && alias.eq_ignore_ascii_case(nickname)
                    })
                {
                    return Err(invalid_request(
                        "Nickname conflicts with another ChatGPT account alias",
                    ));
                }
            }
            AccountProvider::ClaudeSubscription => {
                ClaudeAccountController::new(&config)
                    .validate_selection(&params.account_id)
                    .map_err(|error| invalid_request(error.to_string()))?;
            }
        }
        let provider_id = params.provider.config_id();
        if !nickname.is_empty()
            && config
                .wrangler
                .account_nicknames
                .get(provider_id)
                .is_some_and(|names| {
                    names.iter().any(|(id, name)| {
                        id != &params.account_id && name.to_lowercase() == nickname.to_lowercase()
                    })
                })
        {
            return Err(invalid_request(
                "Nickname already belongs to another account for this provider",
            ));
        }
        let path = vec![
            "wrangler".into(),
            "account_nicknames".into(),
            provider_id.into(),
            params.account_id.clone(),
        ];
        let nickname = (!nickname.is_empty()).then(|| nickname.to_owned());
        let edit = match &nickname {
            Some(name) => ConfigEdit::SetPath {
                segments: path,
                value: toml_edit::value(name),
            },
            None => ConfigEdit::ClearPath { segments: path },
        };
        ConfigEditsBuilder::for_config(&config)
            .with_edits([edit])
            .apply()
            .await
            .map_err(|error| internal_error(format!("Could not save account nickname: {error}")))?;
        Ok(Some(
            AccountNicknameSetResponse {
                provider: params.provider,
                account_id: params.account_id,
                nickname,
            }
            .into(),
        ))
    }
}
