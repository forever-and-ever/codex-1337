//! Credential admission runs after fresh resolution and before catalog dispatch.
use super::OpenAiModelsEndpoint;
use crate::ProviderCredentialGuard;
use codex_login::CodexAuth;
use codex_models_manager::manager::ModelsEndpointClient;
use codex_protocol::error::Result;
use std::sync::Arc;

pub(crate) fn validate(
    guard: Option<&dyn ProviderCredentialGuard>,
    auth: Option<&CodexAuth>,
) -> Result<()> {
    if let Some(guard) = guard {
        guard.validate(auth)?;
    }
    Ok(())
}

impl OpenAiModelsEndpoint {
    pub(crate) fn with_credential_guard(
        mut self,
        guard: Option<Arc<dyn ProviderCredentialGuard>>,
    ) -> Self {
        self.credential_guard = guard;
        self
    }
}

impl ProviderCredentialGuard for codex_login::CapturedAuthSource {
    fn validate(&self, auth: Option<&CodexAuth>) -> Result<()> {
        codex_login::CapturedAuthSource::validate(self)?;
        if self.requires_auth() != auth.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "captured credentials are unavailable",
            )
            .into());
        }
        Ok(())
    }
}

impl OpenAiModelsEndpoint {
    pub(crate) async fn admit_catalog_credentials(
        &self,
    ) -> Result<(
        Option<Arc<codex_login::AuthManager>>,
        Option<Arc<dyn ProviderCredentialGuard>>,
    )> {
        if self.runtime_auth.is_none()
            || self.credential_guard.is_some()
            || self.has_provider_api_key()
        {
            return Ok((self.auth_manager.clone(), self.credential_guard.clone()));
        }
        let Some(manager) = &self.auth_manager else {
            return Ok((None, None));
        };
        // The UI catalog retains its mutable owner. Each actual fetch admits the
        // resolved snapshot once, without a second command/token-source read.
        if self.auth().await.is_none() && manager.auth_cached().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "catalog credential source is unavailable",
            )
            .into());
        }
        let capture = Arc::new(manager.capture_request_source()?);
        Ok((Some(capture.credentials()), Some(capture)))
    }
}
