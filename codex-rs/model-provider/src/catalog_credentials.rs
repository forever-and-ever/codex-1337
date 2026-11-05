//! Credential admission runs after fresh resolution and before catalog dispatch.
use super::OpenAiModelsEndpoint;
use crate::ProviderCredentialGuard;
use codex_login::CodexAuth;
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
