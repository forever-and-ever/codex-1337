//! Captures a credential source, not the current bytes of a refreshable bearer.

use super::AuthManager;
use crate::CodexAuth;
use crate::ExternalAuth;
use crate::ExternalAuthFuture;
use crate::ExternalAuthRefreshContext;
use crate::RefreshTokenError;
use std::io;
use std::sync::Arc;

#[cfg(test)]
#[path = "captured_source_tests.rs"]
mod tests;

enum Source {
    External {
        provider: Arc<dyn ExternalAuth>,
        initial: CodexAuth,
    },
    Static(Option<CodexAuth>),
}

impl ExternalAuth for Source {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        match self {
            Self::External { provider, initial } => Box::pin(async move {
                let auth = provider.resolve().await?;
                validate_principal(initial, &auth)?;
                Ok(auth)
            }),
            Self::Static(auth) => Box::pin(async move {
                auth.clone().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "captured credentials are unavailable",
                    )
                })
            }),
        }
    }

    fn refresh(&self, context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        match self {
            Self::External { provider, initial } => Box::pin(async move {
                let auth = provider.refresh(context).await?;
                validate_principal(initial, &auth)?;
                Ok(auth)
            }),
            Self::Static(_) => Box::pin(async {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "captured static credentials cannot refresh",
                ))
            }),
        }
    }

    fn classify_error(&self, error: io::Error) -> RefreshTokenError {
        match self {
            Self::External { provider, .. } => provider.classify_error(error),
            Self::Static(_) => RefreshTokenError::Transient(error),
        }
    }
}

fn same_opaque_kind(previous: Option<&CodexAuth>, current: Option<&CodexAuth>) -> bool {
    previous.zip(current).is_some_and(|(previous, current)| {
        !previous.is_chatgpt_auth()
            && !current.is_chatgpt_auth()
            && previous.api_auth_mode() == current.api_auth_mode()
    })
}

fn validate_principal(initial: &CodexAuth, current: &CodexAuth) -> io::Result<()> {
    let same = if initial.is_chatgpt_auth() {
        crate::auth::change_state::same_owner(Some(initial), Some(current))
    } else {
        !current.is_chatgpt_auth() && initial.api_auth_mode() == current.api_auth_mode()
    };
    if !same {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "captured credential principal changed",
        ));
    }
    Ok(())
}

// A captured cache borrows the source owner's live policy. It may update bearer
// bytes, but cannot publish a login event or revoke that owner's operations.
#[derive(Default)]
pub(in crate::auth::manager) enum AuthPublication {
    #[default]
    Owner,
    Captured(codex_http_client::NetworkPolicy),
}

impl AuthPublication {
    pub(in crate::auth::manager) fn application_policy(
        &self,
        routes: &crate::AuthRouteConfig,
    ) -> codex_http_client::NetworkPolicy {
        match self {
            Self::Owner => routes.application_network_policy().clone(),
            Self::Captured(policy) => policy.clone(),
        }
    }
    pub(in crate::auth::manager) fn content_policy(
        &self,
        routes: &crate::AuthRouteConfig,
    ) -> codex_http_client::NetworkPolicy {
        match self {
            Self::Owner => routes
                .application_network_policy()
                .clone()
                .for_current_account(),
            Self::Captured(policy) => policy.clone(),
        }
    }
    pub(in crate::auth::manager) fn invalidate(&self, routes: &crate::AuthRouteConfig) {
        if matches!(self, Self::Owner) {
            routes.application_network_policy().invalidate();
        }
    }
}

enum Principal {
    Source {
        mode: Option<codex_protocol::auth::AuthMode>,
    },
    Chatgpt {
        owner_generation: u64,
    },
}

enum CapturedSource {
    Isolated(Arc<Source>),
    Runtime(Arc<dyn ExternalAuth>),
}
impl CapturedSource {
    fn provider(&self) -> Arc<dyn ExternalAuth> {
        match self {
            Self::Isolated(source) => source.clone(),
            Self::Runtime(source) => Arc::clone(source),
        }
    }
    fn requires_auth(&self) -> bool {
        !matches!(self, Self::Isolated(source) if matches!(source.as_ref(), Source::Static(None)))
    }
}

pub struct CapturedAuthSource {
    credentials: Arc<AuthManager>,
    source: CapturedSource,
    principal: Principal,
}

impl std::fmt::Debug for CapturedAuthSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapturedAuthSource")
            .finish_non_exhaustive()
    }
}

impl CapturedAuthSource {
    pub fn credentials(&self) -> Arc<AuthManager> {
        Arc::clone(&self.credentials)
    }

    pub fn validate(&self) -> io::Result<()> {
        if !self
            .credentials
            .external_auth_provider()
            .is_some_and(|source| Arc::ptr_eq(&source, &self.source.provider()))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "captured credential source was replaced",
            ));
        }
        if let Principal::Chatgpt { owner_generation } = self.principal
            && self
                .credentials
                .auth_change_state_receiver()
                .borrow()
                .owner_generation
                != owner_generation
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "captured ChatGPT principal changed",
            ));
        }
        if let Principal::Source { mode } = self.principal
            && self
                .credentials
                .auth_cached()
                .is_some_and(|auth| auth.is_chatgpt_auth() || Some(auth.api_auth_mode()) != mode)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "captured credential principal changed",
            ));
        }
        if let Some(failure) = self
            .credentials
            .auth_cached()
            .and_then(|auth| self.credentials.refresh_failure_for_auth(&auth))
        {
            return Err(io::Error::other(failure));
        }
        Ok(())
    }

    pub fn requires_auth(&self) -> bool {
        self.source.requires_auth()
    }
}

impl AuthManager {
    pub(in crate::auth::manager) fn external_source_owns_rotation(
        &self,
        previous: Option<&CodexAuth>,
        current: Option<&CodexAuth>,
    ) -> bool {
        same_opaque_kind(previous, current)
            // Publication holds the cache lock. Never wait behind a source
            // replacement; a concurrent replacement must revoke.
            && self.external_auth.try_read().is_ok_and(|source| source.is_some())
    }

    pub(in crate::auth::manager) fn replace_external_auth_owner(
        &self,
        source: Arc<dyn ExternalAuth>,
        auth: &CodexAuth,
    ) -> Result<(), RefreshTokenError> {
        let mut slot = self.external_auth.write().map_err(|_| {
            RefreshTokenError::Transient(io::Error::other("external auth lock is poisoned"))
        })?;
        if slot
            .as_ref()
            .is_some_and(|previous| Arc::ptr_eq(previous, &source))
        {
            return Ok(());
        }
        // Match clear_external_auth's source-before-cache order. Cache publication
        // only tries the source lock, so it cannot invert this order. Publish the
        // source transition and freshness fence while holding both locks.
        let cache = self.inner.write().map_err(|_| {
            RefreshTokenError::Transient(io::Error::other("credential cache lock is poisoned"))
        })?;
        let opaque_replacement = same_opaque_kind(cache.auth.as_ref(), Some(auth));
        *slot = Some(source);
        if opaque_replacement {
            self.auth_publication.invalidate(&self.auth_route_config);
        }
        self.auth_change_state_tx.send_modify(|state| {
            // Read freshness does not imply a new principal, login, or runtime actor.
            state.generation += 1;
            if opaque_replacement {
                state.owner_generation += 1;
                state.source_generation += 1;
                state.login_generation += 1;
            }
        });
        self.auth_change_tx.send_modify(|revision| *revision += 1);
        Ok(())
    }

    pub fn capture_auxiliary_source(&self) -> io::Result<CapturedAuthSource> {
        let revision = *self.auth_change_state_receiver().borrow();
        let auth = self.auth_cached();
        let source: Arc<Source> = Arc::new(match self.external_auth_provider() {
            Some(provider) => Source::External {
                provider,
                initial: auth.clone().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "captured credential source is unavailable",
                    )
                })?,
            },
            None => Source::Static(auth.clone()),
        });
        let mut manager = Self::from_auth_state(self.saved_account_config(), false, auth);
        manager.auth_publication =
            AuthPublication::Captured(self.http_client_factory().network_policy().clone());
        *manager
            .external_auth
            .write()
            .map_err(|_| io::Error::other("credential source lock poisoned"))? =
            Some(source.clone());
        if let Some(resolver) = self.workspace_routing_resolver.get() {
            manager.set_workspace_routing_resolver(resolver.clone());
        }
        if *self.auth_change_state_receiver().borrow() != revision {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "credential source changed during capture",
            ));
        }
        let principal = if manager.auth_cached().is_some_and(|auth| {
            auth.get_chatgpt_user_id().is_some() && auth.get_account_id().is_some()
        }) {
            Principal::Chatgpt {
                owner_generation: manager
                    .auth_change_state_receiver()
                    .borrow()
                    .owner_generation,
            }
        } else {
            Principal::Source {
                mode: manager.auth_cached().map(|auth| auth.api_auth_mode()),
            }
        };
        Ok(CapturedAuthSource {
            credentials: Arc::new(manager),
            source: CapturedSource::Isolated(source),
            principal,
        })
    }
}

impl AuthManager {
    pub fn capture_runtime_source(self: &Arc<Self>) -> io::Result<CapturedAuthSource> {
        let source = self
            .external_auth_provider()
            .ok_or_else(|| io::Error::other("runtime credential source is unavailable"))?;
        let principal = if self
            .auth_cached()
            .is_some_and(|auth| auth.is_chatgpt_auth())
        {
            Principal::Chatgpt {
                owner_generation: self.auth_change_state_receiver().borrow().owner_generation,
            }
        } else {
            Principal::Source {
                mode: self.auth_cached().map(|auth| auth.api_auth_mode()),
            }
        };
        Ok(CapturedAuthSource {
            credentials: Arc::clone(self),
            source: CapturedSource::Runtime(source),
            principal,
        })
    }
}

impl AuthManager {
    pub(in crate::auth::manager) fn from_auth_state(
        auth_config: super::super::AuthConfig,
        enable_codex_api_key_env: bool,
        auth: Option<CodexAuth>,
    ) -> Self {
        use super::super::AuthChangeState;
        use super::super::AuthConfig;
        use super::super::CachedAuth;
        use super::super::agent_identity_authapi_base_url;
        use std::sync::Mutex;
        use std::sync::OnceLock;
        use std::sync::RwLock;
        use tokio::sync::Semaphore;
        use tokio::sync::watch;
        let AuthConfig {
            codex_home,
            auth_credentials_store_mode,
            keyring_backend_kind,
            forced_login_method,
            chatgpt_base_url,
            forced_chatgpt_workspace_id,
            managed_auth_policy,
            auth_route_config,
        } = auth_config;
        let agent_identity_authapi_base_url =
            agent_identity_authapi_base_url(chatgpt_base_url.as_deref()).ok();
        Self {
            codex_home,
            account_scope: RwLock::new(None),
            inner: RwLock::new(CachedAuth {
                auth,
                permanent_refresh_failure: None,
            }),
            auth_change_tx: watch::channel(0).0,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            workspace_routing_resolver: OnceLock::new(),
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            keyring_backend_kind,
            forced_login_method,
            forced_chatgpt_workspace_id: RwLock::new(forced_chatgpt_workspace_id),
            managed_auth_policy,
            chatgpt_base_url,
            agent_identity_authapi_base_url,
            refresh_lock: Arc::new(Semaphore::new(1)),
            agent_identity_lock: Semaphore::new(1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(None),
            auth_publication: AuthPublication::Owner,
            workload_identity_selected: false,
            auth_route_config,
        }
    }
}
