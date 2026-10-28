use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClient;
use codex_http_client::HttpClientBuilder;
use codex_http_client::HttpClientFactory;
use serde::Deserialize;
use tokio::sync::Mutex;

use super::AccountId;
use super::AccountStore;
use super::Credentials;
use super::SubscriptionGrant;
use super::storage::Record;
use crate::oauth::ErrorBodyLimit;
use crate::oauth::OAuthClient;
use crate::oauth::OAuthError;
use crate::oauth::RefreshTokenGrant;
use crate::oauth::TokenEncoding;
use crate::oauth::TokenEndpoint;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Anthropic sign-in required")]
    LoginRequired,
    #[error("Anthropic token endpoint rejected refresh (HTTP {0})")]
    Rejected(u16),
    #[error("Anthropic token exchange failed")]
    ExchangeFailed,
    #[error("Anthropic token response is invalid")]
    InvalidResponse,
    #[error("Anthropic account storage failed: {0}")]
    Storage(#[from] io::Error),
}

/// Owns account refresh state across requests, including rotated grants awaiting a failed save.
/// Keep one manager alive for the host; pinned accounts share its pending persistence state.
#[derive(Clone)]
pub struct AuthManager {
    state: Arc<State>,
}

struct State {
    store: AccountStore,
    http: HttpClient,
    token_url: String,
    pending: Mutex<BTreeMap<AccountId, Pending>>,
}

struct Pending {
    previous: Record,
    replacement: Record,
}

/// Captures one identity, not a mutable global selection. It never falls back to another account.
#[derive(Clone)]
pub struct Account {
    manager: AuthManager,
    id: AccountId,
}

impl AuthManager {
    pub fn new(store: AccountStore, http_factory: &HttpClientFactory) -> io::Result<Self> {
        Self::with_endpoint(store, http_factory, super::TOKEN_URL.to_owned())
    }

    pub(super) fn with_endpoint(
        store: AccountStore,
        http_factory: &HttpClientFactory,
        token_url: String,
    ) -> io::Result<Self> {
        let http = HttpClientBuilder::new()
            .without_redirects()
            .without_request_logging()
            .build_respecting_outbound_proxy_policy(
                http_factory,
                &token_url,
                ClientRouteClass::Auth,
            )
            .map_err(|_| io::Error::other("failed to build Anthropic OAuth HTTP client"))?;
        Ok(Self {
            state: Arc::new(State {
                store,
                http,
                token_url,
                pending: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    pub fn pin(&self, id: AccountId) -> Account {
        Account {
            manager: self.clone(),
            id,
        }
    }
}

impl Account {
    pub fn id(&self) -> &AccountId {
        &self.id
    }

    pub async fn credentials(&self) -> Result<Credentials, AuthError> {
        self.resolve(/*rejected*/ None).await
    }

    /// This refreshes credentials only. The caller decides whether a failed inference is replayable.
    pub async fn refresh_after_rejection(
        &self,
        rejected: &Credentials,
    ) -> Result<Credentials, AuthError> {
        if rejected.account_id != self.id {
            return Err(io::Error::other(
                "rejected credentials belong to another Anthropic account",
            )
            .into());
        }
        self.resolve(Some(rejected.grant.access_token.clone()))
            .await
    }

    async fn resolve(&self, rejected: Option<String>) -> Result<Credentials, AuthError> {
        let lock = Arc::new(self.manager.state.store.lock(&self.id).await?);
        let account = self.clone();
        // Once a grant starts, cancellation cannot discard its rotated refresh token or release
        // the cross-process lock before the persistence attempt finishes.
        tokio::spawn(async move { account.resolve_owned(rejected, lock).await })
            .await
            .map_err(|_| io::Error::other("Anthropic credential task failed"))?
    }

    async fn resolve_owned(
        self,
        rejected: Option<String>,
        lock: Arc<std::fs::File>,
    ) -> Result<Credentials, AuthError> {
        let state = &self.manager.state;
        let store = state.store.clone();
        let id = self.id.clone();
        let mut record = tokio::task::spawn_blocking(move || store.load(&id))
            .await
            .map_err(|_| io::Error::other("Anthropic credential read task failed"))?
            .map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    AuthError::LoginRequired
                } else {
                    error.into()
                }
            })?;
        let pending = state.pending.lock().await.remove(&self.id);
        if let Some(pending) = pending
            && pending.previous == record
        {
            record = pending.replacement;
            self.persist(&lock, &pending.previous, &record).await?;
        }
        let reusable = record.grant.usable()
            && rejected
                .as_ref()
                .is_none_or(|token| *token != record.grant.access_token);
        if !reusable {
            let refresh_token = record
                .grant
                .refresh_token
                .as_deref()
                .ok_or(AuthError::LoginRequired)?;
            let response: TokenResponse = OAuthClient::new(
                &state.http,
                TokenEndpoint {
                    url: &state.token_url,
                    client_id: super::CLIENT_ID,
                    encoding: TokenEncoding::Json,
                    timeout: Some(Duration::from_secs(/*secs*/ 20)),
                    error_body_limit: ErrorBodyLimit::Bytes(/*bytes*/ 8192),
                },
            )
            .refresh(RefreshTokenGrant {
                refresh_token,
                resource: None,
                scope: Some(super::OAUTH_SCOPE),
                refresh_reason: None,
            })
            .await
            .map_err(|error| match error {
                OAuthError::Rejected(rejection)
                    if rejection.detail.error_code() == Some("invalid_grant") =>
                {
                    AuthError::LoginRequired
                }
                OAuthError::Rejected(rejection) => AuthError::Rejected(rejection.status.as_u16()),
                OAuthError::Transport(_) => AuthError::ExchangeFailed,
                OAuthError::InvalidResponse => AuthError::InvalidResponse,
            })?;
            let previous = record.clone();
            record = response.into_record(record)?;
            self.persist(&lock, &previous, &record).await?;
        }
        Ok(Credentials {
            account_id: self.id.clone(),
            grant: record.grant,
        })
    }

    async fn persist(
        &self,
        lock: &Arc<std::fs::File>,
        previous: &Record,
        replacement: &Record,
    ) -> Result<(), AuthError> {
        let state = &self.manager.state;
        state.pending.lock().await.insert(
            self.id.clone(),
            Pending {
                previous: previous.clone(),
                replacement: replacement.clone(),
            },
        );
        let store = state.store.clone();
        let id = self.id.clone();
        let record = replacement.clone();
        let lock = Arc::clone(lock);
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            store.save(&id, &record)
        })
        .await
        .map_err(|_| io::Error::other("Anthropic credential write task failed"))??;
        state.pending.lock().await.remove(&self.id);
        Ok(())
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    token_type: Option<String>,
    expires_in: i64,
    account: Option<UpstreamIdentity>,
    organization: Option<UpstreamIdentity>,
}

#[derive(Deserialize)]
struct UpstreamIdentity {
    uuid: String,
}

impl TokenResponse {
    fn into_record(self, mut record: Record) -> Result<Record, AuthError> {
        if self.expires_in <= 0
            || self
                .token_type
                .is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer"))
            || self
                .account
                .is_some_and(|account| account.uuid != record.info.identity.account_uuid())
        {
            return Err(AuthError::InvalidResponse);
        }
        let expires_at = Utc::now()
            .timestamp()
            .checked_add(self.expires_in)
            .ok_or(AuthError::InvalidResponse)?;
        record.grant = SubscriptionGrant::new(
            self.access_token,
            self.refresh_token
                .or_else(|| record.grant.refresh_token.clone())
                .ok_or(AuthError::InvalidResponse)?,
            expires_at,
        )
        .map_err(|_| AuthError::InvalidResponse)?;
        if let Some(organization) = self.organization {
            record.info.organization_uuid = Some(organization.uuid);
        }
        Ok(record)
    }
}
