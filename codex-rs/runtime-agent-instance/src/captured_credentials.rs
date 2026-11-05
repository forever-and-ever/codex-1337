//! A captured owner is checked at credential acquisition, before any registration.
use super::RuntimeAuthError;
use super::RuntimeAuthManager;
use crate::PlatformResourceSelection;
use crate::RegistrationError;
use crate::RuntimeAuth;
use codex_login::CodexAuth;
use codex_login::RefreshTokenError;
use http::StatusCode;

impl RuntimeAuthManager {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "runtime initialization serializes registration and refresh for one credential source"
    )]
    pub async fn auth_with_selection_guarded(
        &self,
        selection: &PlatformResourceSelection,
        validate: impl Fn(&CodexAuth) -> Result<(), RuntimeAuthError> + Send + Sync,
    ) -> Result<RuntimeAuth, RuntimeAuthError> {
        // Preserve the existing owner's serialized registration/recovery algorithm.
        let mut state = self.state.lock().await;
        let (auth, http_client_factory) = self
            .credentials
            .auth_with_http_client_factory()
            .await
            .ok_or(RuntimeAuthError::Unavailable)?;
        validate(&auth)?;
        let mut recovery = self.credentials.unauthorized_recovery();
        let mut result = self
            .auth_once(&mut state, auth, http_client_factory, selection)
            .await;
        while matches!(
            &result,
            Err(RuntimeAuthError::Registration(
                RegistrationError::Rejected {
                    status: StatusCode::UNAUTHORIZED,
                    ..
                }
            ))
        ) && recovery.has_next()
        {
            recovery.next().await.map_err(|error| match error {
                RefreshTokenError::Policy(denied) => RuntimeAuthError::Policy(denied),
                RefreshTokenError::Permanent(_) | RefreshTokenError::Transient(_) => {
                    RuntimeAuthError::RefreshFailed
                }
            })?;
            let (auth, http_client_factory) = self
                .credentials
                .auth_with_http_client_factory()
                .await
                .ok_or(RuntimeAuthError::Unavailable)?;
            validate(&auth)?;
            result = self
                .auth_once(&mut state, auth, http_client_factory, selection)
                .await;
        }
        result
    }
}
