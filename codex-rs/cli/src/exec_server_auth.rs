//! Registry credential rotation follows storage without changing the admitted payer.

use codex_api::AuthError;
use codex_api::AuthProvider;
use codex_api::SharedAuthProvider;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use http::HeaderMap;
use std::sync::Arc;

pub(super) fn reloading_auth(
    credentials: Arc<AuthManager>,
    expected: &CodexAuth,
) -> SharedAuthProvider {
    Arc::new(ReloadingAuth {
        provider: codex_model_provider::auth_provider_from_auth_manager(
            Arc::clone(&credentials),
            expected,
        ),
        credentials,
        expected: expected.clone(),
    })
}

struct ReloadingAuth {
    credentials: Arc<AuthManager>,
    expected: CodexAuth,
    provider: SharedAuthProvider,
}

impl AuthProvider for ReloadingAuth {
    fn add_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), AuthError> {
        Err(AuthError::Transient(
            "Registry authentication requires asynchronous storage admission".into(),
        ))
    }

    fn apply_auth_headers(&self, headers: HeaderMap) -> codex_api::AuthHeadersFuture<'_> {
        Box::pin(async move {
            // Registry timeouts may cancel this waiter, never a credential rotation's persistence.
            let credentials = Arc::clone(&self.credentials);
            tokio::spawn(async move { credentials.reload_with_cached_auth().await })
                .await
                .map_err(|error| {
                    AuthError::Build(format!("Registry credential task failed: {error}"))
                })?
                .map_err(|error| {
                    AuthError::Transient(format!("Registry credentials unavailable: {error}"))
                })?;
            let current = self
                .credentials
                .auth_cached()
                .ok_or_else(|| AuthError::Build("Registry credentials were removed".into()))?;
            if !current.uses_codex_backend()
                || current.get_account_id() != self.expected.get_account_id()
                || current.get_chatgpt_user_id() != self.expected.get_chatgpt_user_id()
                || current.is_workspace_account() != self.expected.is_workspace_account()
            {
                return Err(AuthError::Build("Registry credential owner changed".into()));
            }
            self.provider.apply_auth_headers(headers).await
        })
    }

    fn refresh_after_unauthorized(&self, rejected: HeaderMap) -> codex_api::AuthRefreshFuture<'_> {
        self.provider.refresh_after_unauthorized(rejected)
    }
}
