use std::fmt;
use std::sync::Arc;

use codex_api::AuthProvider;
use codex_api::SharedAuthProvider;
use codex_login::AuthRuntimeConfig;
use codex_login::anthropic::Account;
use codex_login::anthropic::AccountId;
use codex_login::anthropic::AuthManager;
use codex_login::anthropic::Credentials;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use http::HeaderMap;
use http::HeaderValue;

/// Native credentials and the explicitly configured discovery identity.
/// Inference captures its conversation identity independently of this default.
pub struct AnthropicAuthContext {
    manager: Arc<AuthManager>,
    discovery_account: Option<AccountId>,
}

impl fmt::Debug for AnthropicAuthContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicAuthContext")
            .field("discovery_account", &self.discovery_account)
            .finish_non_exhaustive()
    }
}

impl AnthropicAuthContext {
    pub(crate) fn new(
        runtime: &AuthRuntimeConfig,
        discovery_account: Option<String>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            manager: crate::shared_state::process_shared_state().anthropic_auth(runtime)?,
            discovery_account: discovery_account.map(|id| id.parse()).transpose()?,
        })
    }

    pub fn discovery_account_id(&self) -> Option<&AccountId> {
        self.discovery_account.as_ref()
    }

    pub(crate) fn for_account(&self, id: &str) -> Result<Self> {
        Ok(Self {
            manager: Arc::clone(&self.manager),
            discovery_account: Some(id.parse().map_err(|_| {
                CodexErr::InvalidRequest("Invalid Anthropic subscription account identity".into())
            })?),
        })
    }

    pub async fn discovery_auth(&self) -> Result<AnthropicRequestAuth> {
        let id = self.discovery_account.as_ref().ok_or_else(|| {
            CodexErr::InvalidRequest("Select an Anthropic account before discovering models".into())
        })?;
        self.capture_id(id.clone()).await
    }

    pub async fn capture(&self, id: &str) -> Result<AnthropicRequestAuth> {
        self.capture_id(id.parse().map_err(|_| {
            CodexErr::InvalidRequest("Invalid Anthropic subscription account identity".into())
        })?)
        .await
    }

    async fn capture_id(&self, id: AccountId) -> Result<AnthropicRequestAuth> {
        let account = self.manager.pin(id);
        let credentials = account.credentials().await.map_err(|error| {
            CodexErr::InvalidRequest(format!("Anthropic authentication failed: {error}"))
        })?;
        AnthropicRequestAuth::new(account, credentials)
    }
}

/// One request's immutable identity and bearer, retained across dispatch.
pub struct AnthropicRequestAuth {
    account: Account,
    credentials: Credentials,
    auth: SharedAuthProvider,
}

impl AnthropicRequestAuth {
    fn new(account: Account, credentials: Credentials) -> Result<Self> {
        let mut header = HeaderValue::from_str(&format!("Bearer {}", credentials.access_token()))
            .map_err(|_| {
            CodexErr::InvalidRequest("Invalid Anthropic bearer credential".into())
        })?;
        header.set_sensitive(true);
        Ok(Self {
            account,
            credentials,
            auth: Arc::new(AnthropicBearerAuth(header)),
        })
    }

    pub fn auth(&self) -> SharedAuthProvider {
        Arc::clone(&self.auth)
    }

    /// Refreshes only the rejected identity; it never resends an inference request.
    pub async fn refresh_after_rejection(&self) -> Result<Self> {
        let credentials = self
            .account
            .refresh_after_rejection(&self.credentials)
            .await
            .map_err(|error| {
                CodexErr::InvalidRequest(format!("Anthropic authentication failed: {error}"))
            })?;
        Self::new(self.account.clone(), credentials)
    }
}

struct AnthropicBearerAuth(HeaderValue);

impl AuthProvider for AnthropicBearerAuth {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        headers.insert(http::header::AUTHORIZATION, self.0.clone());
    }
}
