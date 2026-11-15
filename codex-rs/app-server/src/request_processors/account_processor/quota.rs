use super::AccountRequestProcessor;
use super::quota_cache::QuotaCache;
use super::quota_cache::QuotaFailure;
use super::quota_cache::QuotaFreshness;
use super::quota_cache::QuotaKey;
use super::quota_cache::QuotaRead;
use super::quota_cache::QuotaUsage;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use codex_app_server_protocol::AccountProvider;
use codex_app_server_protocol::AccountQuota;
use codex_app_server_protocol::AccountQuotaReadParams;
use codex_app_server_protocol::AccountQuotaReadResponse;
use codex_app_server_protocol::AccountQuotaWindow;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_backend_client::Client;
use codex_backend_client::RequestError;
use codex_goal_extension::QuotaError;
use codex_login::AuthManager;
use codex_login::anthropic::AccountStore;
use codex_protocol::goal_boxes::QuotaReading;
use codex_protocol::protocol::RateLimitWindow;
use http::HeaderMap;
use std::sync::Arc;

pub(super) enum QuotaCapability {
    Passive,
    LunaReserve,
}

struct QuotaIdentity {
    id: String,
    email: Option<String>,
    nickname: Option<String>,
}

/// Reads provider account quota through the server's shared quota cache.
#[derive(Clone)]
pub(crate) struct QuotaReader {
    auth_manager: Arc<AuthManager>,
    chatgpt_base_url: String,
    quota_cache: Arc<QuotaCache>,
    runtime_auth: Result<
        Option<(
            Arc<codex_runtime_agent_instance::RuntimeAuthManager>,
            codex_features::RuntimeAuthMode,
        )>,
        String,
    >,
}

impl AccountRequestProcessor {
    pub(crate) fn quota_reader(&self) -> QuotaReader {
        self.quota_reader_for_config(&self.config)
    }

    pub(super) fn quota_reader_for_config(
        &self,
        config: &codex_core::config::Config,
    ) -> QuotaReader {
        QuotaReader {
            auth_manager: Arc::clone(&self.auth_manager),
            chatgpt_base_url: config.chatgpt_base_url.clone(),
            quota_cache: Arc::clone(&self.quota_cache),
            runtime_auth: self
                .thread_manager
                .runtime_auth_for_config(config)
                .map_err(|error| error.to_string()),
        }
    }

    pub(crate) async fn read_account_quota(
        &self,
        params: AccountQuotaReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let config = self
            .config_manager
            .load_non_project_config()
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        let names = config
            .wrangler
            .account_nicknames
            .get(params.provider.config_id());
        let mut accounts: Vec<QuotaIdentity> = match params.provider {
            AccountProvider::OpenRouter => {
                return Err(invalid_request("Use openrouter/key/status"));
            }
            AccountProvider::Openai => self
                .auth_manager
                .saved_accounts()
                .map_err(|_| internal_error("OpenAI account registry unavailable"))?
                .into_iter()
                .map(|account| QuotaIdentity {
                    nickname: names.and_then(|names| names.get(&account.id)).cloned(),
                    id: account.id,
                    email: account.email,
                })
                .collect(),
            AccountProvider::ClaudeSubscription => {
                AccountStore::new(config.codex_home.to_path_buf())
                    .list()
                    .map_err(|_| internal_error("Claude account registry unavailable"))?
                    .into_iter()
                    .map(|account| {
                        let id = account.identity.id().to_string();
                        QuotaIdentity {
                            nickname: names.and_then(|names| names.get(&id)).cloned(),
                            id,
                            email: account.email,
                        }
                    })
                    .collect()
            }
        };
        if let Some(selector) = params.account {
            let selector = selector.trim();
            if selector.is_empty() {
                return Err(invalid_request("Account selector is empty"));
            }
            let exact_id = accounts.iter().any(|account| account.id == selector);
            let alias = (params.provider == AccountProvider::Openai)
                .then(|| config.wrangler.account_aliases.get(selector))
                .flatten();
            accounts.retain(|account| {
                account.id == selector
                    || (!exact_id
                        && (alias == Some(&account.id)
                            || account
                                .nickname
                                .as_deref()
                                .is_some_and(|value| value.eq_ignore_ascii_case(selector))
                            || account
                                .email
                                .as_deref()
                                .is_some_and(|value| value.eq_ignore_ascii_case(selector))))
            });
            match accounts.len() {
                1 => {}
                0 => return Err(invalid_request("No saved account matches that selector")),
                _ => return Err(invalid_request("Account selector is ambiguous; use its ID")),
            }
        }
        let reader = &self.quota_reader_for_config(&config);
        let mut accounts =
            futures::future::join_all(accounts.into_iter().map(|account| async move {
                let result = match params.provider {
                    AccountProvider::OpenRouter => {
                        Err(std::io::Error::other("Use openrouter/key/status"))
                    }
                    AccountProvider::Openai => {
                        reader
                            .read_openai(
                                account.id.clone(),
                                QuotaCapability::Passive,
                                QuotaFreshness::Cached,
                            )
                            .await
                    }
                    AccountProvider::ClaudeSubscription => {
                        reader
                            .read_claude(account.id.clone(), QuotaFreshness::Cached)
                            .await
                    }
                };
                let read = result.unwrap_or_else(|error| QuotaRead {
                    error: Some(error.to_string()),
                    retry_after_sec: 60,
                    ..Default::default()
                });
                let (five_hour, seven_day) = quota_windows(&read);
                AccountQuota {
                    id: account.id,
                    email: account.email,
                    nickname: account.nickname,
                    five_hour: five_hour.map(quota_window),
                    seven_day: seven_day.map(quota_window),
                    captured_at: read.captured_at,
                    retry_after_sec: read.retry_after_sec,
                    error: read.error,
                }
            }))
            .await;
        accounts.sort_by(|a, b| a.email.cmp(&b.email).then_with(|| a.id.cmp(&b.id)));
        Ok(Some(AccountQuotaReadResponse { accounts }.into()))
    }
}

impl QuotaReader {
    pub(super) async fn read_openai(
        &self,
        id: String,
        capability: QuotaCapability,
        freshness: QuotaFreshness,
    ) -> std::io::Result<QuotaRead> {
        let base_url = self.chatgpt_base_url.clone();
        let manager = self.auth_manager.clone();
        let runtime = self.runtime_auth.clone().map_err(std::io::Error::other)?;
        let reserve = matches!(capability, QuotaCapability::LunaReserve);
        self.quota_cache
            .read(
                QuotaKey {
                    provider: AccountProvider::Openai,
                    account: id.clone(),
                    luna_reserve: reserve,
                },
                freshness,
                move || async move {
                    // Resolving auth can rotate an OAuth grant. Its persistence must finish
                    // even if the quota deadline expires; only the usage GET is disposable.
                    let (scoped, auth, factory) = tokio::spawn(async move {
                        let scoped = manager.for_account(&id).await.map_err(|_| QuotaFailure {
                            message: "OpenAI account unavailable".into(),
                            retry_after: None,
                        })?;
                        let scoped = scoped
                            .capture_auxiliary_source()
                            .map_err(|_| QuotaFailure {
                                message: "OpenAI account capture unavailable".into(),
                                retry_after: None,
                            })?
                            .credentials();
                        let (auth, factory) = scoped
                            .auth_with_http_client_factory()
                            .await
                            .ok_or_else(|| QuotaFailure {
                                message: "OpenAI account credentials unavailable".into(),
                                retry_after: None,
                            })?;
                        Ok::<_, QuotaFailure>((scoped, auth, factory))
                    })
                    .await
                    .map_err(|_| QuotaFailure {
                        message: "OpenAI credential read task failed".into(),
                        retry_after: None,
                    })??;
                    let runtime =
                        runtime.map(|(runtime, mode)| (runtime.for_credentials(scoped), mode));
                    let client = Box::pin(Client::from_auth_with_runtime(
                        base_url,
                        &auth,
                        factory,
                        runtime.as_ref(),
                        &HeaderMap::default(),
                    ))
                    .await
                    .map_err(|_| QuotaFailure {
                        message: "OpenAI quota authentication failed".into(),
                        retry_after: None,
                    })?;
                    let usage = if reserve
                        && auth.auth_mode() == codex_protocol::auth::AuthMode::Chatgpt
                        && !auth.is_fedramp_account()
                    {
                        Box::pin(client.get_rate_limits_with_luna_reserve()).await
                    } else {
                        Box::pin(client.get_rate_limits_with_reset_credits()).await
                    }
                    .map_err(|error| {
                        match error.downcast_ref::<RequestError>() {
                            Some(RequestError::UnexpectedStatus {
                                status,
                                retry_after,
                                ..
                            }) => QuotaFailure {
                                message: format!(
                                    "OpenAI quota unavailable (HTTP {})",
                                    status.as_u16()
                                ),
                                retry_after: *retry_after,
                            },
                            _ => QuotaFailure {
                                message: "OpenAI quota request failed".into(),
                                retry_after: None,
                            },
                        }
                    })?;
                    if usage.rate_limits.is_empty()
                        || usage.rate_limits.iter().any(|snapshot| {
                            snapshot
                                .primary
                                .iter()
                                .chain(snapshot.secondary.iter())
                                .any(|window| !window.used_percent.is_finite())
                        })
                    {
                        return Err(QuotaFailure {
                            message: "Invalid OpenAI quota response".into(),
                            retry_after: None,
                        });
                    }
                    Ok(QuotaUsage::Openai(usage))
                },
            )
            .await
    }

    async fn read_claude(
        &self,
        id: String,
        freshness: QuotaFreshness,
    ) -> std::io::Result<QuotaRead> {
        let runtime = self.auth_manager.runtime_config();
        self.quota_cache
            .read(
                QuotaKey {
                    provider: AccountProvider::ClaudeSubscription,
                    account: id.clone(),
                    luna_reserve: false,
                },
                freshness,
                move || async move {
                    let id = id.parse().map_err(|_| QuotaFailure {
                        message: "Invalid Claude account ID".into(),
                        retry_after: None,
                    })?;
                    codex_model_provider::read_anthropic_quota(&runtime, id)
                        .await
                        .map(QuotaUsage::Anthropic)
                        .map_err(|error| QuotaFailure {
                            message: error.message,
                            retry_after: error.retry_after,
                        })
                },
            )
            .await
    }

    /// The weekly window an `OpenAI` account has left: what a percent quotabox watches.
    pub(crate) async fn openai_weekly(&self, id: String) -> Result<QuotaReading, QuotaError> {
        weekly_reading(
            self.read_openai(id, QuotaCapability::Passive, QuotaFreshness::Cached)
                .await,
        )
    }

    /// The weekly window a Claude subscription account has left.
    pub(crate) async fn claude_weekly(&self, id: String) -> Result<QuotaReading, QuotaError> {
        weekly_reading(self.read_claude(id, QuotaFreshness::Cached).await)
    }
}

fn weekly_reading(read: std::io::Result<QuotaRead>) -> Result<QuotaReading, QuotaError> {
    let read = read.map_err(|error| QuotaError::Unreadable(error.to_string()))?;
    if let Some(window) = quota_windows(&read).1 {
        return Ok(QuotaReading::WeeklyPercentLeft {
            percent: quota_window(window).remaining_percent,
        });
    }
    Err(match read.usage {
        Some(_) => {
            QuotaError::Unsupported("This account reports no weekly quota window.".to_string())
        }
        None => QuotaError::Unreadable(
            read.error
                .unwrap_or_else(|| "Quota unavailable".to_string()),
        ),
    })
}

/// An account's five-hour and seven-day windows, as its provider reports them.
fn quota_windows(read: &QuotaRead) -> (Option<&RateLimitWindow>, Option<&RateLimitWindow>) {
    match read.usage.as_ref() {
        Some(QuotaUsage::Anthropic(quota)) => (quota.five_hour.as_ref(), quota.seven_day.as_ref()),
        Some(QuotaUsage::Openai(usage)) => {
            let snapshot = usage
                .rate_limits
                .iter()
                .find(|snapshot| snapshot.limit_id.as_deref() == Some("codex"))
                .or_else(|| usage.rate_limits.first());
            let window = |minutes| {
                snapshot.and_then(|snapshot| {
                    snapshot
                        .primary
                        .iter()
                        .chain(snapshot.secondary.iter())
                        .find(|window| window.window_minutes == Some(minutes))
                })
            };
            (window(300), window(10080))
        }
        None => (None, None),
    }
}

fn quota_window(window: &RateLimitWindow) -> AccountQuotaWindow {
    AccountQuotaWindow {
        remaining_percent: (100.0 - window.used_percent).clamp(0.0, 100.0),
        resets_at: window.resets_at,
    }
}
