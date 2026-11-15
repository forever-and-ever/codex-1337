use super::*;

use codex_app_server_protocol::ResetCreditConfirmation;
use codex_app_server_protocol::ResetCreditScope;
use std::time::Instant;

// Dialog/retry bindings live at most 30 minutes; the 256-entry cap also bounds abandoned reads.

const CONFIRMATION_RETENTION: Duration = Duration::from_secs(30 * 60);
const CONFIRMATION_CAPACITY: usize = 256;
const RECONFIRM: &str = "Reset confirmation is no longer valid. A previous attempt's outcome may be unknown; read usage and reconfirm before starting another attempt.";

struct ResetCreditAuthority {
    config: Arc<Config>,
    auth_manager: Arc<AuthManager>,
    owner_generation: u64,
    scope: ResetCreditScope,
    issued_at: Instant,
}

#[derive(Default)]
pub(super) struct ResetCreditConfirmations {
    // Retries retain the binding. Bound abandoned dialogs without a durable token ledger.
    bindings: HashMap<String, Arc<ResetCreditAuthority>>,
}

impl ResetCreditConfirmations {
    fn insert(&mut self, authority: ResetCreditAuthority) -> ResetCreditConfirmation {
        self.bindings
            .retain(|_, binding| binding.issued_at.elapsed() < CONFIRMATION_RETENTION);
        if self.bindings.len() >= CONFIRMATION_CAPACITY
            && let Some(oldest) = self
                .bindings
                .iter()
                .min_by_key(|(_, binding)| binding.issued_at)
                .map(|(token, _)| token.clone())
        {
            self.bindings.remove(&oldest);
        }
        let confirmation = ResetCreditConfirmation {
            token: uuid::Uuid::new_v4().to_string(),
            scope: authority.scope.clone(),
        };
        self.bindings
            .insert(confirmation.token.clone(), Arc::new(authority));
        confirmation
    }

    fn get(
        &mut self,
        confirmation: &ResetCreditConfirmation,
    ) -> Result<Arc<ResetCreditAuthority>, JSONRPCErrorError> {
        self.bindings
            .retain(|_, binding| binding.issued_at.elapsed() < CONFIRMATION_RETENTION);
        self.bindings
            .get(&confirmation.token)
            .filter(|binding| binding.scope == confirmation.scope)
            .cloned()
            .ok_or_else(|| invalid_request(RECONFIRM))
    }
}

fn reset_credit_scope(
    thread_id: Option<&str>,
    new_thread: Option<&codex_app_server_protocol::NewThreadAccountParams>,
) -> Result<ResetCreditScope, JSONRPCErrorError> {
    match (thread_id, new_thread) {
        (None, None) => Ok(ResetCreditScope::Global),
        (Some(thread_id), None) => Ok(ResetCreditScope::Thread {
            thread_id: thread_id.to_owned(),
        }),
        (None, Some(params)) => Ok(ResetCreditScope::NewThread {
            params: params.clone(),
        }),
        (Some(_), Some(_)) => Err(invalid_request(
            "newThread cannot be combined with threadId",
        )),
    }
}

const RATE_LIMIT_RESET_REQUEST_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 10);
const RATE_LIMIT_RESET_DETAILS_REQUEST_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 5);
#[cfg(debug_assertions)]
const RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR: &str =
    "CODEX_TEST_RATE_LIMIT_RESET_REQUEST_TIMEOUT_MS";

impl AccountRequestProcessor {
    pub(super) async fn reset_credit_read_auth_manager(
        &self,
        params: &GetAccountRateLimitsParams,
    ) -> Result<Arc<AuthManager>, JSONRPCErrorError> {
        if let Some(thread_id) = params.thread_id.as_deref() {
            let id = ThreadId::from_string(thread_id).map_err(|_| invalid_request(RECONFIRM))?;
            let thread = self
                .thread_manager
                .get_thread(id)
                .await
                .map_err(|_| invalid_request(RECONFIRM))?;
            return Ok(thread.auth_manager());
        }
        match self.auth_manager.selected_account_id() {
            Some(id) => self
                .auth_manager
                .for_account(&id)
                .await
                .map_err(|_| invalid_request(RECONFIRM)),
            None => Ok(self.auth_manager.clone()),
        }
    }

    pub(super) async fn bind_reset_credit_confirmation(
        &self,
        params: &GetAccountRateLimitsParams,
        auth_manager: Arc<AuthManager>,
        owner_generation: u64,
    ) -> Option<ResetCreditConfirmation> {
        if auth_manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation
            != owner_generation
        {
            return None;
        }
        let scope =
            reset_credit_scope(params.thread_id.as_deref(), params.new_thread.as_ref()).ok()?;
        let config = Arc::new(self.load_latest_config().await);
        Some(
            self.reset_credit_confirmations
                .lock()
                .await
                .insert(ResetCreditAuthority {
                    config,
                    auth_manager,
                    owner_generation,
                    scope,
                    issued_at: Instant::now(),
                }),
        )
    }

    pub(super) async fn detailed_rate_limit_reset_credits(
        client: &BackendClient,
    ) -> Option<RateLimitResetCreditsSummary> {
        let details = match tokio::time::timeout(
            RATE_LIMIT_RESET_DETAILS_REQUEST_TIMEOUT,
            client.list_rate_limit_reset_credits(),
        )
        .await
        {
            Ok(Ok(details)) => details,
            Ok(Err(err)) => {
                tracing::warn!(
                    "failed to fetch rate limit reset credit details; falling back to the usage response: {err}"
                );
                return None;
            }
            Err(_) => {
                tracing::warn!(
                    "rate limit reset credit detail request timed out; falling back to the usage response"
                );
                return None;
            }
        };

        match rate_limit_reset_credits_from_backend(details) {
            Ok(summary) => Some(summary),
            Err(err) => {
                tracing::warn!(
                    "failed to parse rate limit reset credit details; falling back to the usage response: {err}"
                );
                None
            }
        }
    }

    pub(crate) async fn consume_account_rate_limit_reset_credit(
        &self,
        params: ConsumeAccountRateLimitResetCreditParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        if params.idempotency_key.is_empty() {
            return Err(invalid_request("idempotencyKey must not be empty"));
        }
        if params.credit_id.as_deref().is_some_and(str::is_empty) {
            return Err(invalid_request("creditId must not be empty"));
        }

        let client = self.rate_limit_reset_backend_client(&params).await?;
        let request_timeout = RATE_LIMIT_RESET_REQUEST_TIMEOUT;
        #[cfg(debug_assertions)]
        let request_timeout = std::env::var(RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(request_timeout);
        let response = tokio::time::timeout(request_timeout, async {
            match params.credit_id.as_deref() {
                Some(credit_id) => {
                    client
                        .consume_rate_limit_reset_credit_by_id(&params.idempotency_key, credit_id)
                        .await
                }
                None => {
                    client
                        .consume_rate_limit_reset_credit(&params.idempotency_key)
                        .await
                }
            }
        })
        .await
        .map_err(|_| internal_error("rate limit reset consume timed out"))?
        .map_err(|err| internal_error(format!("failed to consume rate limit reset: {err}")))?;
        let outcome = match response.code {
            BackendConsumeRateLimitResetCreditCode::Reset => {
                ConsumeAccountRateLimitResetCreditOutcome::Reset
            }
            BackendConsumeRateLimitResetCreditCode::NothingToReset => {
                ConsumeAccountRateLimitResetCreditOutcome::NothingToReset
            }
            BackendConsumeRateLimitResetCreditCode::NoCredit => {
                ConsumeAccountRateLimitResetCreditOutcome::NoCredit
            }
            BackendConsumeRateLimitResetCreditCode::AlreadyRedeemed => {
                ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed
            }
        };
        Ok(Some(
            ConsumeAccountRateLimitResetCreditResponse { outcome }.into(),
        ))
    }

    async fn rate_limit_reset_backend_client(
        &self,
        params: &ConsumeAccountRateLimitResetCreditParams,
    ) -> Result<BackendClient, JSONRPCErrorError> {
        let scope = reset_credit_scope(params.thread_id.as_deref(), params.new_thread.as_ref())?;
        let confirmation = params.confirmation.as_ref().ok_or_else(|| {
            invalid_request("Reset confirmation required; read account/rateLimits/read and reconfirm before spending a reset credit.")
        })?;
        if confirmation.scope != scope {
            return Err(invalid_request(RECONFIRM));
        }
        let authority = self
            .reset_credit_confirmations
            .lock()
            .await
            .get(confirmation)?;
        if authority.auth_manager.is_account_scoped() {
            authority.auth_manager.reload().await;
        }
        let credentials = Self::captured_backend_credentials(&authority.auth_manager)?;
        let Some((auth, http_client_factory)) = credentials.auth_with_http_client_factory().await
        else {
            return Err(invalid_request(RECONFIRM));
        };
        if let ResetCreditScope::Thread { thread_id } = &authority.scope {
            let id = ThreadId::from_string(thread_id).map_err(|_| invalid_request(RECONFIRM))?;
            let thread = self
                .thread_manager
                .get_thread(id)
                .await
                .map_err(|_| invalid_request(RECONFIRM))?;
            if !Arc::ptr_eq(&thread.auth_manager(), &authority.auth_manager) {
                return Err(invalid_request(RECONFIRM));
            }
        }
        if authority
            .auth_manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation
            != authority.owner_generation
            || !auth.uses_codex_backend()
        {
            return Err(invalid_request(RECONFIRM));
        }
        // The factory revokes captured credentials if their owner changes after validation.
        self.backend_client_for_config(&authority.config, &credentials, &auth, http_client_factory)
            .await
    }
}

fn rate_limit_reset_credits_from_backend(
    details: BackendRateLimitResetCreditsDetails,
) -> Result<RateLimitResetCreditsSummary, String> {
    let credits = details
        .credits
        .into_iter()
        .map(rate_limit_reset_credit_from_backend)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RateLimitResetCreditsSummary {
        available_count: details.available_count,
        credits: Some(credits),
    })
}

fn rate_limit_reset_credit_from_backend(
    credit: BackendRateLimitResetCreditDetails,
) -> Result<RateLimitResetCredit, String> {
    let reset_type = match credit.reset_type.as_str() {
        "codex_rate_limits" => RateLimitResetType::CodexRateLimits,
        _ => RateLimitResetType::Unknown,
    };
    let status = match credit.status.as_str() {
        "available" => RateLimitResetCreditStatus::Available,
        "redeeming" => RateLimitResetCreditStatus::Redeeming,
        "redeemed" => RateLimitResetCreditStatus::Redeemed,
        _ => RateLimitResetCreditStatus::Unknown,
    };
    let granted_at = rate_limit_reset_credit_timestamp(&credit.granted_at)
        .map_err(|err| format!("invalid granted_at for credit `{}`: {err}", credit.id))?;
    let expires_at = credit
        .expires_at
        .as_deref()
        .map(rate_limit_reset_credit_timestamp)
        .transpose()
        .map_err(|err| format!("invalid expires_at for credit `{}`: {err}", credit.id))?;

    Ok(RateLimitResetCredit {
        id: credit.id,
        reset_type,
        status,
        granted_at,
        expires_at,
        title: credit.title,
        description: credit.description,
    })
}

fn rate_limit_reset_credit_timestamp(timestamp: &str) -> Result<i64, String> {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|timestamp| timestamp.timestamp())
        .map_err(|err| format!("failed to parse timestamp `{timestamp}`: {err}"))
}
