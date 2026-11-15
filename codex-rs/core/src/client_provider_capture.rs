//! An admitted auxiliary owns its payer for its whole lifetime. This is execution
//! authority, not the configured provider/picker facade, and deliberately has no Deref.

use super::ClientRouting;
use super::CurrentClientSetup;
use super::ModelClient;
use crate::agent::control::ProviderAccountSelection;
use crate::config::Config;
use codex_login::AuthManager;
use codex_model_provider::ProviderAuthContext;
use codex_model_provider::SharedModelProvider;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::KeyName;
use codex_protocol::error::CodexErr;
use codex_protocol::openai_models::ModelInfo;
use std::sync::Arc;

impl super::ModelClientSession {
    pub(super) fn reset_credential_bound_turn_state(
        &mut self,
        owner_changed: bool,
        runtime_id: Option<&str>,
    ) {
        // Opaque source rotation may preserve policy ownership while replacing
        // the runtime actor. Sticky turn state cannot cross that runtime binding.
        if owner_changed || self.websocket_session.runtime_id.as_deref() != runtime_id {
            self.turn_state = Arc::new(std::sync::OnceLock::new());
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderCaptureError {
    #[error("select a provider account before starting auxiliary work")]
    AccountRequired,
    #[error("the provider credential owner changed while auxiliary work was captured")]
    OwnerChanged,
    #[error("runtime authentication requires an already account-scoped credential owner")]
    RuntimeOwnerNotScoped,
    #[error("the captured provider no longer matches the requested provider")]
    ProviderChanged,
    #[error(transparent)]
    Configuration(#[from] CodexErr),
    #[error(transparent)]
    Credentials(#[from] std::io::Error),
}

#[derive(Clone, Debug)]
pub struct CapturedProvider(Arc<ExecutionProvider>);

pub struct CapturedAuthMetadata {
    pub mode: codex_protocol::auth::AuthMode,
    pub account_id: Option<String>,
    pub account_email: Option<String>,
}

#[derive(Debug)]
enum ExecutionProvider {
    Responses {
        provider: SharedModelProvider,
        owner: Arc<CredentialOwner>,
    },
    Claude {
        provider: SharedModelProvider,
        account: codex_login::anthropic::AccountId,
    },
    OpenRouter {
        provider: SharedModelProvider,
        key: KeyName,
    },
}

#[derive(Debug)]
enum CredentialOwner {
    Anonymous(SharedModelProvider),
    ConfiguredBearer(codex_login::CodexAuth),
    // A private immutable captured source may rotate opaque bearer bytes. Its source,
    // not those bytes, is the authority. No mutable source manager escapes this module.
    Source(codex_login::CapturedAuthSource),
    // OAuth rotation preserves owner_generation. Login replacement, logout, removal,
    // and principal changes invalidate the admitted job, never select another payer.
    Principal {
        credentials: Arc<AuthManager>,
        owner_generation: u64,
    },
}

impl CredentialOwner {
    fn same_authority(&self, other: &Self) -> bool {
        if self.validate().is_err() || other.validate().is_err() {
            return false;
        }
        match (self, other) {
            (Self::Anonymous(left), Self::Anonymous(right)) => Arc::ptr_eq(left, right),
            (Self::ConfiguredBearer(left), Self::ConfiguredBearer(right)) => left == right,
            (Self::Source(left), Self::Source(right)) => left.same_authority(right),
            (
                Self::Principal {
                    credentials: left,
                    owner_generation: left_generation,
                },
                Self::Principal {
                    credentials: right,
                    owner_generation: right_generation,
                },
            ) => left_generation == right_generation && left.same_credential_authority(right),
            _ => false,
        }
    }

    fn credentials(&self) -> Option<Arc<AuthManager>> {
        match self {
            Self::Anonymous(_) | Self::ConfiguredBearer(_) => None,
            Self::Source(source) => Some(source.credentials()),
            Self::Principal { credentials, .. } => Some(Arc::clone(credentials)),
        }
    }

    fn validate(&self) -> Result<(), ProviderCaptureError> {
        match self {
            Self::Principal {
                credentials,
                owner_generation,
            } if credentials
                .auth_change_state_receiver()
                .borrow()
                .owner_generation
                != *owner_generation =>
            {
                Err(ProviderCaptureError::OwnerChanged)
            }
            Self::Source(source) => Ok(source.validate()?),
            _ => Ok(()),
        }
    }

    fn requires_auth(&self) -> bool {
        match self {
            Self::Principal { .. } | Self::ConfiguredBearer(_) => true,
            Self::Source(source) => source.requires_auth(),
            Self::Anonymous(_) => false,
        }
    }

    fn validate_auth(
        &self,
        auth: Option<&codex_login::CodexAuth>,
    ) -> Result<(), ProviderCaptureError> {
        self.validate()?;
        if let Self::ConfiguredBearer(expected) = self {
            return if auth == Some(expected) {
                Ok(())
            } else {
                Err(ProviderCaptureError::OwnerChanged)
            };
        }
        match auth {
            Some(_) if matches!(self, Self::Anonymous(_)) => {
                return Err(ProviderCaptureError::OwnerChanged);
            }
            None if self.requires_auth() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "captured credentials are unavailable",
                )
                .into());
            }
            Some(auth)
                if self.credentials().is_some_and(|credentials| {
                    credentials.auth_cached().as_ref() != Some(auth)
                }) =>
            {
                return Err(ProviderCaptureError::OwnerChanged);
            }
            Some(_) | None => {}
        }
        Ok(())
    }
}

impl codex_model_provider::ProviderCredentialGuard for CredentialOwner {
    fn validate(&self, auth: Option<&codex_login::CodexAuth>) -> codex_protocol::error::Result<()> {
        Ok(self.validate_auth(auth)?)
    }

    fn configured_bearer(&self) -> Option<codex_login::CodexAuth> {
        match self {
            Self::ConfiguredBearer(auth) => Some(auth.clone()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum StartCredentials {
    #[default]
    Inherit,
    Captured(CapturedProvider),
}

impl CapturedProvider {
    /// Admit an explicitly selected Responses provider before auxiliary discovery or execution.
    pub fn capture_responses(provider: SharedModelProvider) -> Result<Self, ProviderCaptureError> {
        if provider.info().adapter != ModelProviderAdapter::OpenaiCompatible {
            return Err(ProviderCaptureError::ProviderChanged);
        }
        if let Some(auth) = codex_model_provider::configured_bearer_credentials(provider.info())? {
            let provider = match provider.auth_manager() {
                Some(source) => provider.for_auth_context(ProviderAuthContext {
                    credentials: AuthManager::from_api_key(
                        &auth.get_token()?,
                        source.runtime_config(),
                    ),
                    runtime_auth: provider.credential_runtime().cloned(),
                })?,
                None if provider.credential_runtime().is_some() => {
                    return Err(ProviderCaptureError::RuntimeOwnerNotScoped);
                }
                None => provider,
            };
            let owner = Arc::new(CredentialOwner::ConfiguredBearer(auth));
            let provider = provider.with_credential_guard(owner.clone())?;
            let captured = Self(Arc::new(ExecutionProvider::Responses { provider, owner }));
            captured.validate()?;
            return Ok(captured);
        }
        let Some(source) = provider.auth_manager() else {
            let owner = Arc::new(CredentialOwner::Anonymous(Arc::clone(&provider)));
            let provider = provider.with_credential_guard(owner.clone())?;
            return Ok(Self(Arc::new(ExecutionProvider::Responses {
                provider,
                owner,
            })));
        };
        let revision = *source.auth_change_state_receiver().borrow();
        let principal_owner = |credentials: Arc<AuthManager>| {
            let owner_generation = credentials
                .auth_change_state_receiver()
                .borrow()
                .owner_generation;
            CredentialOwner::Principal {
                credentials,
                owner_generation,
            }
        };
        let owner = if provider.credential_runtime().is_some() && source.has_external_auth() {
            CredentialOwner::Source(source.capture_runtime_source()?)
        } else if !source.has_external_auth()
            && (source.selected_account_id().is_some()
                || source
                    .auth_cached()
                    .is_some_and(|auth| auth.is_chatgpt_auth()))
        {
            CredentialOwner::Source(source.capture_job_source()?)
        } else if provider.credential_runtime().is_some() {
            principal_owner(Arc::clone(&source))
        } else {
            CredentialOwner::Source(source.capture_auxiliary_source()?)
        };
        if source
            .auth_change_state_receiver()
            .borrow()
            .owner_generation
            != revision.owner_generation
        {
            return Err(ProviderCaptureError::OwnerChanged);
        }
        let owner = Arc::new(owner);
        let credentials = match owner.as_ref() {
            CredentialOwner::Principal { credentials, .. } => Arc::clone(credentials),
            CredentialOwner::Source(source) => source.credentials(),
            CredentialOwner::Anonymous(_) | CredentialOwner::ConfiguredBearer(_) => unreachable!(),
        };
        let provider = provider
            .for_auth_context(ProviderAuthContext {
                credentials,
                runtime_auth: provider.credential_runtime().cloned(),
            })?
            .with_credential_guard(owner.clone())?;
        let captured = Self(Arc::new(ExecutionProvider::Responses { provider, owner }));
        captured.validate()?;
        Ok(captured)
    }

    pub fn info(&self) -> &ModelProviderInfo {
        self.provider().info()
    }

    pub fn provider(&self) -> &SharedModelProvider {
        match self.0.as_ref() {
            ExecutionProvider::Responses { provider, .. }
            | ExecutionProvider::Claude { provider, .. }
            | ExecutionProvider::OpenRouter { provider, .. } => provider,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ProviderCaptureError> {
        match self.0.as_ref() {
            ExecutionProvider::Responses { owner, .. } => owner.validate(),
            _ => Ok(()),
        }
    }

    pub(crate) fn selection(&self) -> Option<ProviderAccountSelection> {
        match self.0.as_ref() {
            ExecutionProvider::Claude { account, .. } => {
                Some(ProviderAccountSelection::ClaudeSubscription {
                    id: account.to_string(),
                })
            }
            ExecutionProvider::OpenRouter { key, .. } => {
                Some(ProviderAccountSelection::OpenRouter { key: key.clone() })
            }
            ExecutionProvider::Responses { owner, .. } => {
                let credentials = match owner.as_ref() {
                    CredentialOwner::Anonymous(_) | CredentialOwner::ConfiguredBearer(_) => {
                        return None;
                    }
                    CredentialOwner::Source(source) => {
                        return source
                            .credentials()
                            .selected_account_id()
                            .map(|id| ProviderAccountSelection::Chatgpt { id });
                    }
                    CredentialOwner::Principal { credentials, .. } => credentials,
                };
                credentials
                    .selected_account_id()
                    .map(|id| ProviderAccountSelection::Chatgpt { id })
            }
        }
    }

    pub(crate) fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.provider().auth_manager()
    }

    /// Compares admitted authority, never bearer bytes or a reusable account name alone.
    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        if self.validate().is_err() || other.validate().is_err() {
            return false;
        }
        let info = |capture: &Self| ModelProviderInfo {
            request_max_retries: None,
            stream_max_retries: None,
            supports_websockets: false,
            ..capture.info().clone()
        };
        if info(self) != info(other) {
            return false;
        }
        let same_runtime = match (
            self.provider().credential_runtime(),
            other.provider().credential_runtime(),
        ) {
            (Some((left, left_mode)), Some((right, right_mode))) => {
                left_mode == right_mode && left.same_registration_authority(right)
            }
            (None, None) => true,
            _ => false,
        };
        if !same_runtime {
            return false;
        }
        match (self.0.as_ref(), other.0.as_ref()) {
            (
                ExecutionProvider::Responses { owner: left, .. },
                ExecutionProvider::Responses { owner: right, .. },
            ) => {
                left.same_authority(right)
                    && match (
                        self.provider().auth_manager(),
                        other.provider().auth_manager(),
                    ) {
                        (Some(left), Some(right)) => {
                            left.http_client_factory().network_policy()
                                == right.http_client_factory().network_policy()
                        }
                        (None, None) => {
                            Arc::ptr_eq(left, right)
                                || matches!(
                                    (left.as_ref(), right.as_ref()),
                                    (CredentialOwner::Anonymous(_), CredentialOwner::Anonymous(_))
                                )
                                || Arc::ptr_eq(self.provider(), other.provider())
                        }
                        _ => false,
                    }
            }
            // Native account/key names do not prove the credential incarnation.
            // Only the same immutable native provider binding proves reuse.
            (
                ExecutionProvider::Claude { account: left, .. },
                ExecutionProvider::Claude { account: right, .. },
            ) => left == right && Arc::ptr_eq(self.provider(), other.provider()),
            (
                ExecutionProvider::OpenRouter { key: left, .. },
                ExecutionProvider::OpenRouter { key: right, .. },
            ) => left == right && Arc::ptr_eq(self.provider(), other.provider()),
            _ => false,
        }
    }

    pub(crate) fn for_transport(
        &self,
        info: &ModelProviderInfo,
    ) -> Result<Self, ProviderCaptureError> {
        self.matches(&ModelProviderInfo {
            request_max_retries: self.info().request_max_retries,
            stream_max_retries: self.info().stream_max_retries,
            supports_websockets: self.info().supports_websockets,
            ..info.clone()
        })?;
        let provider = self.provider().for_transport(
            info.request_max_retries,
            info.stream_max_retries,
            info.supports_websockets,
        )?;
        let execution = match self.0.as_ref() {
            ExecutionProvider::Responses { owner, .. } => ExecutionProvider::Responses {
                provider,
                owner: Arc::clone(owner),
            },
            ExecutionProvider::Claude { account, .. } => ExecutionProvider::Claude {
                provider,
                account: account.clone(),
            },
            ExecutionProvider::OpenRouter { key, .. } => ExecutionProvider::OpenRouter {
                provider,
                key: key.clone(),
            },
        };
        Ok(Self(Arc::new(execution)))
    }

    pub(crate) fn matches(&self, info: &ModelProviderInfo) -> Result<(), ProviderCaptureError> {
        self.validate()?;
        if self.info() != info {
            return Err(ProviderCaptureError::ProviderChanged);
        }
        Ok(())
    }

    pub fn memory_extraction_preferred_model(&self) -> Option<&'static str> {
        self.provider().memory_extraction_preferred_model()
    }

    pub fn memory_consolidation_preferred_model(&self) -> Option<&'static str> {
        self.provider().memory_consolidation_preferred_model()
    }

    pub(crate) fn catalog_provider(
        &self,
        config: &Config,
    ) -> Result<SharedModelProvider, ProviderCaptureError> {
        self.validate()?;
        self.matches(&config.model_provider)?;
        Ok(match self.0.as_ref() {
            ExecutionProvider::Claude { provider, account } => {
                provider.for_anthropic_account(&account.to_string())?
            }
            _ => Arc::clone(self.provider()),
        })
    }

    pub fn models_manager(
        &self,
        config: &Config,
    ) -> Result<SharedModelsManager, ProviderCaptureError> {
        Ok(self
            .catalog_provider(config)?
            .models_manager(config.models_cache_path(), config.model_catalog.clone()))
    }

    pub async fn model_info(&self, config: &Config, model: &str) -> anyhow::Result<ModelInfo> {
        let manager = self.models_manager(config)?;
        if self.info().adapter == ModelProviderAdapter::ClaudeSubscription {
            // Best-effort picker lookup must not replace a recoverable sign-in failure
            // with missing metadata. The caller receives the original typed CodexErr.
            let catalog = manager
                .authoritative_catalog(
                    RefreshStrategy::OnlineIfUncached,
                    config.http_client_factory(),
                )
                .await?;
            if !catalog
                .models
                .iter()
                .any(|entry| entry.slug == model && entry.anthropic.is_some())
            {
                return Err(CodexErr::InvalidRequest(
                    "Cannot load authoritative Anthropic auxiliary model metadata for this account"
                        .into(),
                )
                .into());
            }
        }
        let info = manager
            .get_model_info(model, &config.to_models_manager_config())
            .await;
        self.validate()?;
        Ok(Arc::unwrap_or_clone(info.model))
    }
}

impl ModelClient {
    pub fn capture_provider(&self) -> Result<CapturedProvider, ProviderCaptureError> {
        let provider = Arc::clone(&self.state.provider);
        let captured = match provider.info().adapter {
            ModelProviderAdapter::ClaudeSubscription => {
                let Some(ProviderAccountSelection::ClaudeSubscription { id }) =
                    self.provider_account_selection()
                else {
                    return Err(ProviderCaptureError::AccountRequired);
                };
                ExecutionProvider::Claude {
                    provider: provider.for_anthropic_account(&id)?,
                    account: id.parse().map_err(std::io::Error::other)?,
                }
            }
            ModelProviderAdapter::OpenRouter => {
                let Some(ProviderAccountSelection::OpenRouter { key }) =
                    self.provider_account_selection()
                else {
                    return Err(ProviderCaptureError::AccountRequired);
                };
                ExecutionProvider::OpenRouter { provider, key }
            }
            ModelProviderAdapter::OpenaiCompatible => {
                return CapturedProvider::capture_responses(provider);
            }
        };
        let captured = CapturedProvider(Arc::new(captured));
        captured.validate()?;
        Ok(captured)
    }

    pub fn with_captured_provider(
        mut self,
        captured: CapturedProvider,
    ) -> Result<Self, ProviderCaptureError> {
        captured.matches(self.state.provider.info())?;
        let state = Arc::get_mut(&mut self.state).ok_or(ProviderCaptureError::OwnerChanged)?;
        state.provider = Arc::clone(captured.provider());
        self = self.with_provider_account(captured.selection().map(Arc::new));
        self.execution = StartCredentials::Captured(captured);
        Ok(self)
    }

    pub(super) fn validate_captured_provider(&self) -> codex_protocol::error::Result<()> {
        if let StartCredentials::Captured(captured) = &self.execution {
            captured
                .validate()
                .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
        }
        Ok(())
    }

    pub(super) async fn current_client_setup(
        &self,
        routing: ClientRouting,
    ) -> codex_protocol::error::Result<CurrentClientSetup> {
        self.validate_captured_provider()?;
        let setup = self.current_client_setup_unchecked(routing).await?;
        if let StartCredentials::Captured(captured) = &self.execution {
            captured.validate_responses_auth(setup.auth.as_ref())?;
        }
        Ok(setup)
    }
}

impl StartCredentials {
    pub(crate) fn models_manager(
        &self,
        config: &Config,
        catalog: &crate::thread_manager::model_catalog::ThreadModelCatalog,
        originator: &str,
    ) -> Result<Option<SharedModelsManager>, ProviderCaptureError> {
        match self {
            Self::Inherit => Ok(None),
            Self::Captured(captured) => {
                catalog.for_captured(config, captured, originator).map(Some)
            }
        }
    }
}

impl CapturedProvider {
    pub(crate) fn prepare_spawn(
        &self,
        config: &Config,
        control: &crate::agent::control::AgentControlInit,
    ) -> Result<Option<Arc<AuthManager>>, ProviderCaptureError> {
        self.matches(&config.model_provider)?;
        if let Some(selection) = self.selection() {
            control
                .runtime()
                .account_selection
                .select(&config.model_provider_id, selection);
        }
        Ok(self.auth_manager())
    }

    pub async fn request_auth(
        &self,
    ) -> Result<Option<codex_login::CodexAuth>, ProviderCaptureError> {
        self.validate()?;
        if let ExecutionProvider::Responses { owner, .. } = self.0.as_ref()
            && let CredentialOwner::ConfiguredBearer(auth) = owner.as_ref()
        {
            return Ok(Some(auth.clone()));
        }
        let auth = match self.auth_manager() {
            Some(credentials) => credentials.auth().await,
            None => None,
        };
        self.validate_responses_auth(auth.as_ref())?;
        Ok(auth)
    }

    pub fn auth_metadata(&self) -> Option<CapturedAuthMetadata> {
        let auth = self.auth_manager()?.auth_cached()?;
        Some(CapturedAuthMetadata {
            mode: auth.auth_mode(),
            account_id: auth.get_account_id(),
            account_email: auth.get_account_email(),
        })
    }
}

impl ModelClient {
    pub(crate) fn with_start_credentials(
        self,
        credentials: &StartCredentials,
    ) -> Result<Self, ProviderCaptureError> {
        match credentials {
            StartCredentials::Inherit => Ok(self),
            StartCredentials::Captured(captured) => self.with_captured_provider(captured.clone()),
        }
    }

    pub(crate) fn has_captured_provider(&self) -> bool {
        matches!(self.execution, StartCredentials::Captured(_))
    }

    pub(crate) fn require_mutable_provider(&self) -> std::io::Result<()> {
        if self.has_captured_provider() {
            return Err(std::io::Error::other(
                "An admitted auxiliary job cannot select another credential owner",
            ));
        }
        Ok(())
    }
}

impl From<ProviderCaptureError> for CodexErr {
    fn from(error: ProviderCaptureError) -> Self {
        match error {
            ProviderCaptureError::Configuration(error) => error,
            ProviderCaptureError::Credentials(error) => {
                let error = match error.downcast::<Self>() {
                    Ok(error) => return error,
                    Err(error) => error,
                };
                if let Some(cause) = error.get_ref() {
                    if let Some(failure) =
                        cause.downcast_ref::<codex_protocol::auth::RefreshTokenFailedError>()
                    {
                        return Self::RefreshTokenFailed(failure.clone());
                    }
                    if let Some(codex_login::RefreshTokenError::Permanent(failure)) =
                        cause.downcast_ref::<codex_login::RefreshTokenError>()
                    {
                        return Self::RefreshTokenFailed(failure.clone());
                    }
                }
                Self::Io(error)
            }
            error => Self::InvalidRequest(error.to_string()),
        }
    }
}

impl ModelClient {
    pub(crate) fn start_credentials(&self) -> StartCredentials {
        self.execution.clone()
    }
}

impl CapturedProvider {
    pub fn codex_api_key_env_enabled(&self) -> bool {
        self.auth_manager()
            .is_some_and(|credentials| credentials.codex_api_key_env_enabled())
    }
}

impl CapturedProvider {
    fn validate_responses_auth(
        &self,
        auth: Option<&codex_login::CodexAuth>,
    ) -> Result<(), ProviderCaptureError> {
        match self.0.as_ref() {
            ExecutionProvider::Responses { owner, .. } => owner.validate_auth(auth),
            _ => self.validate(),
        }
    }
}

impl ModelClient {
    pub(super) fn activate_runtime_auth(
        mut self,
        manager: Arc<codex_runtime_agent_instance::RuntimeAuthManager>,
        mode: codex_features::RuntimeAuthMode,
    ) -> codex_protocol::error::Result<Self> {
        let info = self.state.provider.info();
        codex_model_provider::validate_runtime_destination(info.base_url.as_deref())?;
        codex_model_provider::select_runtime_provider(
            info,
            self.auth_manager().as_ref(),
            &manager,
        )?;
        let credentials = self.auth_manager().ok_or_else(|| {
            CodexErr::InvalidRequest("runtime credentials are unavailable".into())
        })?;
        let provider = self.state.provider.for_auth_context(ProviderAuthContext {
            credentials,
            runtime_auth: Some((manager, mode)),
        })?;
        Arc::get_mut(&mut self.state)
            .ok_or_else(|| {
                CodexErr::InvalidRequest(
                    "runtime auth must be configured before sharing the model client".into(),
                )
            })?
            .provider = provider;
        Ok(self)
    }
}
