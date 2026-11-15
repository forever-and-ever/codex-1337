//! Bind credential sources to shared runtime registrations.

use crate::AgentHostAuthorization;
use crate::PlatformResourceSelection;
use crate::RegistrationError;
use crate::RuntimeAuth;
use crate::RuntimeAuthBinding;
use crate::RuntimeAuthError;
use crate::RuntimeRegistrationMetadata;
use crate::binding::RegistrationTarget;
use codex_login::AuthManager;
use codex_login::AuthRuntimeConfig;
use codex_login::CodexAuth;
use std::sync::Arc;
use url::Url;

/// Shares registration state across requests using the same credential source and context.
pub struct RuntimeAuthManager {
    login: Arc<RuntimeAuthBinding>,
    registrations: Arc<std::sync::Mutex<Vec<CredentialRegistration>>>,
}

enum CredentialRegistration {
    Provider(Arc<RuntimeAuthBinding>),
    ApiKey(Arc<RuntimeAuthBinding>),
}

impl std::fmt::Debug for RuntimeAuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAuthManager").finish_non_exhaustive()
    }
}

impl RuntimeAuthManager {
    /// Whether these views retain the very same admitted registration state.
    pub fn same_registration_authority(&self, other: &Self) -> bool {
        self.login.shares_registration(&other.login)
    }

    pub fn uses_credentials(&self, credentials: &Arc<AuthManager>) -> bool {
        self.login.uses_credentials(credentials)
    }

    pub fn new(
        credentials: Arc<AuthManager>,
        host: AgentHostAuthorization,
        metadata: RuntimeRegistrationMetadata,
        registration_url: Url,
    ) -> Self {
        Self::from_registration(
            credentials,
            RegistrationTarget::Explicit {
                host,
                url: registration_url,
            },
            metadata,
        )
    }

    /// Resolve the ChatGPT registration origin only when a first-party request needs a runtime.
    pub fn for_chatgpt(
        credentials: Arc<AuthManager>,
        installation_id: String,
        base_url: String,
        metadata: RuntimeRegistrationMetadata,
    ) -> Self {
        Self::from_registration(
            credentials,
            RegistrationTarget::ChatGpt {
                installation_id,
                base_url,
            },
            metadata,
        )
    }

    fn from_registration(
        credentials: Arc<AuthManager>,
        registration: RegistrationTarget,
        metadata: RuntimeRegistrationMetadata,
    ) -> Self {
        Self {
            login: Arc::new(RuntimeAuthBinding::new(credentials, registration, metadata)),
            registrations: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Construct a Codex runtime owner without registering it or generating its key.
    pub fn for_codex(
        credentials: Arc<AuthManager>,
        chatgpt_base_url: &str,
        installation_id: &str,
    ) -> Self {
        Self::for_chatgpt(
            credentials,
            installation_id.to_owned(),
            chatgpt_base_url.to_owned(),
            RuntimeRegistrationMetadata {
                harness: Some("codex".to_string()),
                harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                role: crate::RuntimeRole::Orchestrator,
            },
        )
    }

    pub fn registration_url(&self) -> Result<Url, RegistrationError> {
        self.login.registration_url()
    }

    pub async fn auth(&self) -> Result<RuntimeAuth, RuntimeAuthError> {
        self.admit_binding(&self.login).await?.auth().await
    }

    pub async fn auth_with_selection(
        &self,
        selection: &PlatformResourceSelection,
    ) -> Result<RuntimeAuth, RuntimeAuthError> {
        self.admit_binding(&self.login)
            .await?
            .auth_with_selection(selection)
            .await
    }

    pub async fn auth_for(
        &self,
        expected: &CodexAuth,
        selection: &PlatformResourceSelection,
    ) -> Result<RuntimeAuth, RuntimeAuthError> {
        self.admit_binding(&self.login)
            .await?
            .auth_for(expected, selection)
            .await
    }

    pub async fn auth_for_source(
        &self,
        source_generation: u64,
        selection: &PlatformResourceSelection,
    ) -> Result<RuntimeAuth, RuntimeAuthError> {
        self.admit_binding(&self.login)
            .await?
            .auth_for_source(source_generation, selection)
            .await
    }

    /// Admit one reusable client/job source; each request still resolves that source.
    pub async fn for_job(self: &Arc<Self>) -> Result<Arc<Self>, RuntimeAuthError> {
        let credentials = &self.login.credentials;
        if credentials.is_source_captured() {
            return Ok(Arc::clone(self));
        }
        credentials
            .auth()
            .await
            .ok_or(RuntimeAuthError::Unavailable)?;
        let captured = credentials.capture_job_source().map_err(capture_error)?;
        Ok(self.for_credentials(captured.credentials()))
    }

    /// Admit an actual job request; mutable owner selection is not a runtime actor.
    pub async fn admit_binding(
        &self,
        binding: &Arc<RuntimeAuthBinding>,
    ) -> Result<Arc<RuntimeAuthBinding>, RuntimeAuthError> {
        let credentials = &binding.credentials;
        if credentials.is_source_captured() {
            return Ok(Arc::clone(binding));
        }
        let auth = credentials
            .auth()
            .await
            .ok_or(RuntimeAuthError::Unavailable)?;
        let owner = credentials
            .auth_owner_for(&auth)
            .ok_or(RuntimeAuthError::Changed)?;
        let captured = credentials
            .capture_request_source()
            .map_err(capture_error)?;
        Ok(self.bind(captured.credentials()).for_issuer(owner))
    }

    /// Select a credential source while retaining the shared registration authority.
    pub fn for_credentials(self: &Arc<Self>, credentials: Arc<AuthManager>) -> Arc<Self> {
        if Arc::ptr_eq(&self.login.credentials, &credentials) {
            return Arc::clone(self);
        }
        Arc::new(Self {
            login: self.bind(credentials),
            registrations: Arc::clone(&self.registrations),
        })
    }

    /// Bind a login or command-backed credential without changing its refresh lifecycle.
    pub fn bind(&self, credentials: Arc<AuthManager>) -> Arc<RuntimeAuthBinding> {
        if self.login.uses_credentials(&credentials) {
            return self.login.for_credentials(credentials);
        }
        let mut registrations = self
            .registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(binding) = registrations.iter().find_map(|entry| match entry {
            CredentialRegistration::Provider(binding) if binding.uses_credentials(&credentials) => {
                Some(binding)
            }
            _ => None,
        }) {
            return binding.for_credentials(credentials);
        }
        let binding = Arc::new(RuntimeAuthBinding::new(
            credentials,
            self.login.registration.clone(),
            self.login.metadata.clone(),
        ));
        registrations.push(CredentialRegistration::Provider(Arc::clone(&binding)));
        binding
    }

    /// An explicit key is independent of login storage and command output, even when bytes match.
    pub fn bind_api_key(
        &self,
        api_key: &str,
        runtime: &AuthRuntimeConfig,
    ) -> Arc<RuntimeAuthBinding> {
        let mut registrations = self
            .registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(binding) = registrations.iter().find_map(|entry| match entry {
            CredentialRegistration::ApiKey(binding)
                if binding
                    .credentials
                    .auth_cached()
                    .as_ref()
                    .and_then(CodexAuth::api_key)
                    == Some(api_key)
                    && binding.credentials.runtime_config() == *runtime =>
            {
                Some(binding)
            }
            _ => None,
        }) {
            return Arc::clone(binding);
        }
        let binding = Arc::new(RuntimeAuthBinding::new(
            AuthManager::from_api_key(api_key, runtime.clone()),
            self.login.registration.clone(),
            self.login.metadata.clone(),
        ));
        registrations.push(CredentialRegistration::ApiKey(Arc::clone(&binding)));
        binding
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;

fn capture_error(error: std::io::Error) -> RuntimeAuthError {
    if let Some(error) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<codex_login::auth::RefreshTokenFailedError>())
    {
        RuntimeAuthError::RefreshTokenFailed(error.clone())
    } else {
        RuntimeAuthError::Changed
    }
}
