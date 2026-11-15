//! Fork routing and eligibility are not methods of the upstream provider trait.
use super::ConfiguredModelProvider;
use super::ModelProvider;
use super::SharedModelProvider;
use crate::AnthropicAuthContext;
use codex_model_provider_info::ModelProviderAdapter;
use codex_protocol::error::CodexErr;
use std::ops::Deref;
use std::sync::Arc;

/// Resolve configured bearer bytes once when admitting auxiliary work.
pub fn configured_bearer_credentials(
    info: &codex_model_provider_info::ModelProviderInfo,
) -> codex_protocol::error::Result<Option<codex_login::CodexAuth>> {
    Ok(crate::auth::bearer_auth_for_provider(info)?
        .and_then(|bearer| bearer.token)
        .map(|token| codex_login::CodexAuth::from_api_key(&token)))
}

pub trait ProviderCredentialGuard: std::fmt::Debug + Send + Sync {
    fn validate(&self, auth: Option<&codex_login::CodexAuth>) -> codex_protocol::error::Result<()>;

    /// A configured bearer can be captured without inventing a storage owner.
    fn configured_bearer(&self) -> Option<codex_login::CodexAuth> {
        None
    }
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

    fn for_transport(
        &self,
        _request_max_retries: Option<u64>,
        _stream_max_retries: Option<u64>,
        _supports_websockets: bool,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        Err(CodexErr::UnsupportedOperation(
            "provider cannot derive captured transport settings".into(),
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
        self.credential_runtime_auth.as_ref()
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
            crate::select_runtime_provider(&self.info, Some(&context.credentials), runtime)?;
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
        provider
            .credential_runtime_auth
            .clone_from(&context.runtime_auth);
        provider.runtime_auth = select_runtime(self.info.base_url.as_deref());
        provider.catalog_runtime_auth = select_runtime(
            self.info
                .model_catalog_url
                .as_deref()
                .map(String::as_str)
                .or(self.info.base_url.as_deref()),
        );
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

    fn for_transport(
        &self,
        request_max_retries: Option<u64>,
        stream_max_retries: Option<u64>,
        supports_websockets: bool,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        let mut provider = self.clone();
        provider.info.request_max_retries = request_max_retries;
        provider.info.stream_max_retries = stream_max_retries;
        provider.info.supports_websockets = supports_websockets;
        Ok(ProviderHandle::configured(provider))
    }
}
#[derive(Clone, Debug)]
pub struct ProviderHandle(Arc<dyn ForkModelProvider>);

impl AsRef<dyn ModelProvider> for ProviderHandle {
    fn as_ref(&self) -> &(dyn ModelProvider + 'static) {
        self.0.as_ref()
    }
}
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

    pub fn for_transport(
        &self,
        request_max_retries: Option<u64>,
        stream_max_retries: Option<u64>,
        supports_websockets: bool,
    ) -> codex_protocol::error::Result<SharedModelProvider> {
        self.0
            .for_transport(request_max_retries, stream_max_retries, supports_websockets)
    }
}
// Configured/discovery state, never captured execution authority.
impl Deref for ProviderHandle {
    type Target = dyn ModelProvider;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}
