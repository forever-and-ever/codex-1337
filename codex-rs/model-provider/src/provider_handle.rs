//! Fork routing and eligibility are not methods of the upstream provider trait.
use super::ConfiguredModelProvider;
use super::ModelProvider;
use super::SharedModelProvider;
use crate::AnthropicAuthContext;
use codex_model_provider_info::ModelProviderAdapter;
use codex_protocol::error::CodexErr;
use std::ops::Deref;
use std::sync::Arc;

pub trait ProviderCredentialGuard: std::fmt::Debug + Send + Sync {
    fn validate(&self, auth: Option<&codex_login::CodexAuth>) -> codex_protocol::error::Result<()>;
}

pub trait ForkModelProvider: ModelProvider {
    fn credential_runtime(
        &self,
    ) -> Option<&(
        Arc<codex_runtime_agent_instance::RuntimeAuthManager>,
        codex_features::RuntimeAuthMode,
    )> {
        self.runtime_auth()
    }
    fn anthropic_auth_context(&self) -> std::io::Result<Option<Arc<AnthropicAuthContext>>> {
        Ok(None)
    }
    fn for_anthropic_account(
        &self,
        _account_id: &str,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        Err(CodexErr::InvalidRequest(
            "Provider does not support Anthropic account-scoped discovery".into(),
        ))
    }
    fn for_auth_context(
        &self,
        _context: crate::ProviderAuthContext,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        Err(CodexErr::UnsupportedOperation(
            "provider does not support captured credentials".into(),
        ))
    }
    fn credential_guard(&self) -> Option<&dyn ProviderCredentialGuard> {
        None
    }

    fn with_credential_guard(
        &self,
        _guard: Arc<dyn ProviderCredentialGuard>,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        Err(CodexErr::UnsupportedOperation(
            "provider cannot bind captured credential validation".into(),
        ))
    }
}
impl ForkModelProvider for ConfiguredModelProvider {
    fn credential_runtime(
        &self,
    ) -> Option<&(
        Arc<codex_runtime_agent_instance::RuntimeAuthManager>,
        codex_features::RuntimeAuthMode,
    )> {
        self.runtime_auth
            .as_ref()
            .or(self.catalog_runtime_auth.as_ref())
    }
    fn anthropic_auth_context(&self) -> std::io::Result<Option<Arc<AnthropicAuthContext>>> {
        self.anthropic_auth_context
            .clone()
            .transpose()
            .map_err(std::io::Error::other)
    }
    fn for_anthropic_account(
        &self,
        account_id: &str,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        let context = self.anthropic_auth_context()?.ok_or_else(|| {
            CodexErr::InvalidRequest(
                "Provider does not support Anthropic account-scoped discovery".into(),
            )
        })?;
        let mut provider = self.clone();
        provider.anthropic_auth_context = Some(Ok(Arc::new(context.for_account(account_id)?)));
        Ok(ProviderHandle::configured(provider))
    }
    fn for_auth_context(
        &self,
        context: crate::ProviderAuthContext,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        let mut provider = self.clone();
        if let Some((runtime, _)) = &context.runtime_auth {
            crate::validate_runtime_provider(&self.info, Some(&context.credentials), runtime)?;
        }
        let select_runtime = |destination: Option<&str>| {
            context.runtime_auth.clone().filter(|_| {
                self.info.adapter == ModelProviderAdapter::OpenaiCompatible
                    && destination.is_none_or(|url| {
                        codex_http_client::classify_openai_api_url(url)
                            != codex_http_client::OpenAiDestination::Other
                    })
            })
        };
        // A catalog-only runtime must retain its original credential pair too.
        if self.runtime_auth.is_none()
            && let Some((runtime, _)) = &self.catalog_runtime_auth
        {
            crate::validate_runtime_provider(&self.info, Some(&context.credentials), runtime)?;
        }
        provider.runtime_auth.clone_from(&context.runtime_auth);
        provider.catalog_runtime_auth =
            if self.runtime_auth.is_none() && context.runtime_auth.is_none() {
                self.catalog_runtime_auth.clone()
            } else {
                select_runtime(
                    self.info
                        .model_catalog_url
                        .as_deref()
                        .map(String::as_str)
                        .or(self.info.base_url.as_deref()),
                )
            };
        provider.auth_manager = Some(context.credentials);
        Ok(ProviderHandle::configured(provider))
    }
    fn credential_guard(&self) -> Option<&dyn ProviderCredentialGuard> {
        self.credential_guard.as_deref()
    }

    fn with_credential_guard(
        &self,
        guard: Arc<dyn ProviderCredentialGuard>,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        let mut provider = self.clone();
        provider.credential_guard = Some(guard);
        Ok(ProviderHandle::configured(provider))
    }
}
#[derive(Clone, Debug)]
pub struct ProviderHandle(Arc<dyn ForkModelProvider>);
impl ProviderHandle {
    pub(super) fn configured(provider: ConfiguredModelProvider) -> SharedModelProvider {
        Self::custom(Arc::new(provider))
    }
    pub fn custom(provider: Arc<dyn ForkModelProvider>) -> SharedModelProvider {
        Arc::new(Self(provider))
    }
    pub fn anthropic_auth_context(&self) -> std::io::Result<Option<Arc<AnthropicAuthContext>>> {
        self.0.anthropic_auth_context()
    }
    pub fn for_anthropic_account(
        &self,
        account_id: &str,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        self.0.for_anthropic_account(account_id)
    }
    pub fn memory_extraction_preferred_model(&self) -> Option<&'static str> {
        match self.info().adapter {
            ModelProviderAdapter::OpenRouter => None,
            ModelProviderAdapter::ClaudeSubscription => Some("claude-haiku-5-5"),
            ModelProviderAdapter::OpenaiCompatible => {
                Some(self.0.memory_extraction_preferred_model())
            }
        }
    }
    pub fn memory_consolidation_preferred_model(&self) -> Option<&'static str> {
        match self.info().adapter {
            ModelProviderAdapter::OpenRouter => None,
            ModelProviderAdapter::ClaudeSubscription => Some("claude-sonnet-5-5"),
            ModelProviderAdapter::OpenaiCompatible => {
                Some(self.0.memory_consolidation_preferred_model())
            }
        }
    }
    pub fn for_auth_context(
        &self,
        context: crate::ProviderAuthContext,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        self.0.for_auth_context(context)
    }
    pub fn credential_runtime(
        &self,
    ) -> Option<&(
        Arc<codex_runtime_agent_instance::RuntimeAuthManager>,
        codex_features::RuntimeAuthMode,
    )> {
        self.0.credential_runtime()
    }
    pub fn credential_guard(&self) -> Option<&dyn ProviderCredentialGuard> {
        self.0.credential_guard()
    }

    pub fn with_credential_guard(
        &self,
        guard: Arc<dyn ProviderCredentialGuard>,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        self.0.with_credential_guard(guard)
    }
}
// Configured/discovery state, never captured execution authority.
impl Deref for ProviderHandle {
    type Target = dyn ModelProvider;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}
