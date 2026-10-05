use super::AccountRequestProcessor;
use super::quota_cache::QuotaFailure;
use super::quota_cache::QuotaFreshness;
use super::quota_cache::QuotaKey;
use super::quota_cache::QuotaRead;
use super::quota_cache::QuotaUsage;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use codex_app_server_protocol::AccountQuota;
use codex_app_server_protocol::AccountQuotaReadParams;
use codex_app_server_protocol::AccountQuotaReadResponse;
use codex_app_server_protocol::AccountQuotaWindow;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::SubscriptionAccountProvider;
use codex_backend_client::Client;
use codex_backend_client::RequestError;
use codex_login::anthropic::AccountStore;
use codex_protocol::protocol::RateLimitWindow;

pub(super) enum QuotaCapability {
    Passive,
    LunaReserve,
}

struct QuotaIdentity {
    id: String,
    email: Option<String>,
    nickname: Option<String>,
}

impl AccountRequestProcessor {
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
            SubscriptionAccountProvider::Openai => self
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
            SubscriptionAccountProvider::ClaudeSubscription => {
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
            let alias = (params.provider == SubscriptionAccountProvider::Openai)
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
        let mut accounts =
            futures::future::join_all(accounts.into_iter().map(|account| async move {
                let result = match params.provider {
                    SubscriptionAccountProvider::Openai => {
                        self.read_openai_quota(
                            account.id.clone(),
                            QuotaCapability::Passive,
                            QuotaFreshness::Cached,
                        )
                        .await
                    }
                    SubscriptionAccountProvider::ClaudeSubscription => {
                        let runtime = self.auth_manager.runtime_config();
                        let id = account.id.clone();
                        self.quota_cache
                            .read(
                                QuotaKey {
                                    provider: params.provider,
                                    account: account.id.clone(),
                                    luna_reserve: false,
                                },
                                QuotaFreshness::Cached,
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
                };
                let read = result.unwrap_or_else(|error| QuotaRead {
                    error: Some(error.to_string()),
                    retry_after_sec: 60,
                    ..Default::default()
                });
                let (five_hour, seven_day) = match read.usage.as_ref() {
                    Some(QuotaUsage::Anthropic(quota)) => {
                        (quota.five_hour.as_ref(), quota.seven_day.as_ref())
                    }
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
                };
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

    pub(super) async fn read_openai_quota(
        &self,
        id: String,
        capability: QuotaCapability,
        freshness: QuotaFreshness,
    ) -> std::io::Result<QuotaRead> {
        let base_url = self.config.chatgpt_base_url.clone();
        let manager = self.auth_manager.clone();
        let reserve = matches!(capability, QuotaCapability::LunaReserve);
        self.quota_cache
            .read(
                QuotaKey {
                    provider: SubscriptionAccountProvider::Openai,
                    account: id.clone(),
                    luna_reserve: reserve,
                },
                freshness,
                move || async move {
                    // Resolving auth can rotate an OAuth grant. Its persistence must finish
                    // even if the quota deadline expires; only the usage GET is disposable.
                    let (auth, factory) = tokio::spawn(async move {
                        let scoped = manager.for_account(&id).await.map_err(|_| QuotaFailure {
                            message: "OpenAI account unavailable".into(),
                            retry_after: None,
                        })?;
                        scoped
                            .auth_with_http_client_factory()
                            .await
                            .ok_or_else(|| QuotaFailure {
                                message: "OpenAI account credentials unavailable".into(),
                                retry_after: None,
                            })
                    })
                    .await
                    .map_err(|_| QuotaFailure {
                        message: "OpenAI credential read task failed".into(),
                        retry_after: None,
                    })??;
                    let client = Client::from_auth(base_url, &auth, factory);
                    let usage = if reserve
                        && auth.auth_mode() == codex_protocol::auth::AuthMode::Chatgpt
                        && !auth.is_fedramp_account()
                    {
                        client.get_rate_limits_with_luna_reserve().await
                    } else {
                        client.get_rate_limits_with_reset_credits().await
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
}

fn quota_window(window: &RateLimitWindow) -> AccountQuotaWindow {
    AccountQuotaWindow {
        remaining_percent: (100.0 - window.used_percent).clamp(0.0, 100.0),
        resets_at: window.resets_at,
    }
}
