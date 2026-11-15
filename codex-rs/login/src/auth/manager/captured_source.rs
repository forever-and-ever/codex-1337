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
    Unavailable,
    Account {
        owner: AccountSource,
        initial: Option<CodexAuth>,
    },
    Request {
        initial: CodexAuth,
        current: std::sync::RwLock<CodexAuth>,
        refresh: RefreshSource,
    },
}

enum RefreshSource {
    External {
        provider: Arc<dyn ExternalAuth>,
        lane: Arc<tokio::sync::Semaphore>,
    },
    Account(AccountSource),
}

struct AccountSource {
    credentials: Arc<AuthManager>,
    issuer: std::sync::Weak<AuthManager>,
}

impl AccountSource {
    async fn refresh(&self, expected: &CodexAuth) -> Result<(), RefreshTokenError> {
        let result = self.credentials.refresh_token_if_unchanged(expected).await;
        // Publish rejection of this exact grant, never resolve through the issuer.
        // A selector that has moved to another account cannot receive A's failure.
        if let Err(RefreshTokenError::Permanent(error)) = &result
            && let Some(issuer) = self.issuer.upgrade()
        {
            issuer.record_permanent_refresh_failure_if_unchanged(expected, error);
        }
        result
    }
}

impl ExternalAuth for Source {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        match self {
            Self::External { provider, initial } => Box::pin(async move {
                let auth = provider.resolve().await?;
                validate_principal(initial, &auth)?;
                Ok(auth)
            }),
            Self::Account { owner, initial } => Box::pin(async move {
                let initial = initial.as_ref().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        crate::auth::manager::permanent_external_auth_error(
                            "captured account is unavailable",
                        ),
                    )
                })?;
                // A workspace grant has no personal selector. Reload its private,
                // admitted owner, never the conversation's mutable default owner.
                if !owner.credentials.is_account_scoped() {
                    owner.credentials.reload_with_cached_auth().await?;
                }
                let auth = owner.credentials.auth().await.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        crate::auth::manager::permanent_external_auth_error(
                            "captured account is unavailable",
                        ),
                    )
                })?;
                if let Some(error) = owner.credentials.refresh_failure_for_auth(&auth) {
                    return Err(io::Error::other(RefreshTokenError::Permanent(error)));
                }
                validate_principal(initial, &auth)?;
                Ok(auth)
            }),
            Self::Request { current, .. } => Box::pin(async move {
                current
                    .read()
                    .map(|auth| auth.clone())
                    .map_err(|_| io::Error::other("captured request credentials are unavailable"))
            }),
            Self::Unavailable => Box::pin(async {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    crate::auth::manager::permanent_external_auth_error(
                        "captured account is unavailable",
                    ),
                ))
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
            Self::Account { owner, initial } => Box::pin(async move {
                let initial = initial.as_ref().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        crate::auth::manager::permanent_external_auth_error(
                            "captured account is unavailable",
                        ),
                    )
                })?;
                let expected = owner.credentials.auth_cached().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        crate::auth::manager::permanent_external_auth_error(
                            "captured account is unavailable",
                        ),
                    )
                })?;
                owner.refresh(&expected).await.map_err(io::Error::other)?;
                let auth = owner.credentials.auth_cached().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        crate::auth::manager::permanent_external_auth_error(
                            "captured account is unavailable",
                        ),
                    )
                })?;
                validate_principal(initial, &auth)?;
                Ok(auth)
            }),
            Self::Request {
                initial,
                current,
                refresh,
            } => Box::pin(async move {
                let expected = current
                    .read()
                    .map_err(|_| io::Error::other("captured request credentials are unavailable"))?
                    .clone();
                let auth = match refresh {
                    RefreshSource::External { provider, lane } => {
                        let _guard = lane.acquire().await.map_err(|_| {
                            io::Error::other("credential refresh lane is unavailable")
                        })?;
                        let latest = provider.resolve().await?;
                        validate_principal(initial, &latest)?;
                        if AuthManager::auths_equal_for_refresh(Some(&expected), Some(&latest)) {
                            provider.refresh(context).await?
                        } else {
                            latest
                        }
                    }
                    RefreshSource::Account(owner) => {
                        owner.refresh(&expected).await.map_err(io::Error::other)?;
                        owner.credentials.auth_cached().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                crate::auth::manager::permanent_external_auth_error(
                                    "captured account is unavailable",
                                ),
                            )
                        })?
                    }
                };
                validate_principal(initial, &auth)?;
                *current.write().map_err(|_| {
                    io::Error::other("captured request credentials are unavailable")
                })? = auth.clone();
                Ok(auth)
            }),
            Self::Static(_) | Self::Unavailable => Box::pin(async {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "captured static credentials cannot refresh",
                ))
            }),
        }
    }

    fn classify_error(&self, error: io::Error) -> RefreshTokenError {
        match error
            .get_ref()
            .and_then(|cause| cause.downcast_ref::<RefreshTokenError>())
        {
            Some(RefreshTokenError::Permanent(failure)) => {
                return RefreshTokenError::Permanent(failure.clone());
            }
            Some(RefreshTokenError::Policy(denied)) => return RefreshTokenError::Policy(*denied),
            _ => {}
        }
        match self {
            Self::External { provider, .. }
            | Self::Request {
                refresh: RefreshSource::External { provider, .. },
                ..
            } => provider.classify_error(error),
            Self::Static(_) | Self::Unavailable | Self::Account { .. } | Self::Request { .. } => {
                RefreshTokenError::Transient(error)
            }
        }
    }
}

fn same_opaque_kind(previous: Option<&CodexAuth>, current: Option<&CodexAuth>) -> bool {
    previous.zip(current).is_some_and(|(previous, current)| {
        !previous.is_chatgpt_auth()
            && !current.is_chatgpt_auth()
            && previous.api_auth_mode() == current.api_auth_mode()
            && previous.api_workload_identity() == current.api_workload_identity()
            && previous.get_chatgpt_user_id() == current.get_chatgpt_user_id()
            && previous.get_account_id() == current.get_account_id()
    })
}

fn validate_principal(initial: &CodexAuth, current: &CodexAuth) -> io::Result<()> {
    let same = if initial.is_chatgpt_auth() {
        AuthManager::auths_equal_for_refresh(Some(initial), Some(current))
            || crate::auth::change_state::same_owner(Some(initial), Some(current))
    } else {
        same_opaque_kind(Some(initial), Some(current))
    };
    if !same {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            crate::auth::manager::permanent_external_auth_error(
                "captured credential principal changed",
            ),
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
    Captured {
        policy: codex_http_client::NetworkPolicy,
        requirement: SourceRequirement,
    },
}

pub(in crate::auth::manager) enum SourceRequirement {
    Anonymous,
    Required,
}

impl AuthPublication {
    pub(in crate::auth::manager) fn application_policy(
        &self,
        routes: &crate::AuthRouteConfig,
    ) -> codex_http_client::NetworkPolicy {
        match self {
            Self::Owner => routes.application_network_policy().clone(),
            Self::Captured { policy, .. } => policy.clone(),
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
            Self::Captured { policy, .. } => policy.clone(),
        }
    }
    pub(in crate::auth::manager) fn invalidate(&self, routes: &crate::AuthRouteConfig) {
        if matches!(self, Self::Owner) {
            routes.application_network_policy().invalidate();
        }
    }
}

struct Principal {
    initial: Option<CodexAuth>,
    revision: PrincipalRevision,
}

enum PrincipalRevision {
    Owner(u64),
    Source(u64),
}

impl Principal {
    fn capture(credentials: &AuthManager) -> Self {
        let initial = credentials.auth_cached();
        let state = *credentials.auth_change_state_receiver().borrow();
        let named = initial.as_ref().is_some_and(|auth| {
            auth.is_chatgpt_auth()
                || auth.api_workload_identity().is_some()
                || auth.get_chatgpt_user_id().is_some()
                || auth.get_account_id().is_some()
        });
        Self {
            initial,
            revision: if named {
                PrincipalRevision::Owner(state.owner_generation)
            } else {
                PrincipalRevision::Source(state.source_generation)
            },
        }
    }

    fn validate(&self, credentials: &AuthManager) -> io::Result<()> {
        let state = *credentials.auth_change_state_receiver().borrow();
        let current = match self.revision {
            PrincipalRevision::Owner(revision) => revision == state.owner_generation,
            PrincipalRevision::Source(revision) => revision == state.source_generation,
        };
        if !current {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "captured credential principal changed",
            ));
        }
        match (self.initial.as_ref(), credentials.auth_cached()) {
            (Some(initial), Some(current)) => validate_principal(initial, &current),
            (None, None) => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "captured credential principal changed",
            )),
        }
    }
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

    pub fn same_authority(&self, other: &Self) -> bool {
        self.validate().is_ok()
            && other.validate().is_ok()
            && self
                .credentials
                .same_credential_authority(&other.credentials)
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
        self.principal.validate(&self.credentials)?;
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
    pub(in crate::auth::manager) fn credential_origin(&self) -> Arc<()> {
        Arc::clone(
            &self
                .credential_origin
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Fresh captures share authority only while their source and principal remain admitted.
    pub fn same_credential_authority(&self, other: &Self) -> bool {
        if self.is_account_scoped() != other.is_account_scoped()
            || matches!(self.auth_publication, AuthPublication::Owner)
                != matches!(other.auth_publication, AuthPublication::Owner)
            || !Arc::ptr_eq(&self.credential_origin(), &other.credential_origin())
        {
            return false;
        }
        let state = *self.auth_change_state_receiver().borrow();
        let other_state = *other.auth_change_state_receiver().borrow();
        if state.source_generation != other_state.source_generation
            || state.login_generation != other_state.login_generation
            || self.selected_account_id() != other.selected_account_id()
        {
            return false;
        }
        let auth = self.auth_cached();
        let other_auth = other.auth_cached();
        auth == other_auth
            || crate::auth::change_state::same_owner(auth.as_ref(), other_auth.as_ref())
            || (self.has_external_auth()
                && other.has_external_auth()
                && auth
                    .as_ref()
                    .zip(other_auth.as_ref())
                    .is_some_and(|(auth, other)| {
                        !auth.is_chatgpt_auth()
                            && !other.is_chatgpt_auth()
                            && auth.api_auth_mode() == other.api_auth_mode()
                    }))
    }

    pub(in crate::auth::manager) fn external_source_owns_rotation(
        &self,
        previous: Option<&CodexAuth>,
        current: Option<&CodexAuth>,
    ) -> bool {
        same_opaque_kind(previous, current)
            && previous.is_none_or(|auth| {
                auth.api_workload_identity().is_none()
                    || crate::auth::source::same_source(previous, current)
            })
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
        *self
            .credential_origin
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(());
        *slot = Some(source);
        if opaque_replacement {
            self.auth_publication
                .invalidate(cache.account.routes(&self.auth_route_config));
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
        self.capture_source(|auth| {
            Ok(Arc::new(match self.external_auth_provider() {
                Some(_)
                    if auth.is_none()
                        && matches!(self.auth_publication, AuthPublication::Captured { .. }) =>
                {
                    match self.auth_publication {
                        AuthPublication::Captured {
                            requirement: SourceRequirement::Required,
                            ..
                        } => Source::Unavailable,
                        _ => Source::Static(None),
                    }
                }
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
            }))
        })
    }

    /// Admit a job without retaining a conversation's mutable saved-account selector.
    pub fn capture_job_source(self: &Arc<Self>) -> io::Result<CapturedAuthSource> {
        if self.has_external_auth() {
            return self.capture_auxiliary_source();
        }
        let auth = self.auth_cached();
        let stored_workspace = matches!(auth.as_ref(),
            Some(CodexAuth::Chatgpt(stored)) if auth.as_ref().is_some_and(CodexAuth::is_workspace_account)
                && !stored.storage().is_ephemeral()
        );
        if self.selected_account_id().is_none() && !stored_workspace {
            return self.capture_auxiliary_source();
        }
        let owner = self.frozen_account_owner()?;
        self.capture_source(|auth| {
            Ok(Arc::new(Source::Account {
                owner: AccountSource {
                    credentials: owner,
                    issuer: Arc::downgrade(self),
                },
                initial: auth.clone(),
            }))
        })
    }

    pub fn is_source_captured(&self) -> bool {
        matches!(self.auth_publication, AuthPublication::Captured { .. })
    }

    /// Freeze the already resolved credential snapshot for one request, without
    /// resolving an external source a second time.
    pub fn capture_request_source(self: &Arc<Self>) -> io::Result<CapturedAuthSource> {
        let revision = *self.auth_change_state_receiver().borrow();
        let initial = self.auth_cached();
        let refresh = if let Some(provider) = self.external_auth_provider() {
            Some(RefreshSource::External {
                provider,
                lane: Arc::clone(&self.refresh_lock),
            })
        } else if self.selected_account_id().is_some()
            || matches!(initial.as_ref(),
                Some(CodexAuth::Chatgpt(stored)) if initial.as_ref().is_some_and(CodexAuth::is_workspace_account)
                    && !stored.storage().is_ephemeral()
            )
        {
            let owner = self.frozen_account_owner()?;
            let auth = owner.auth_cached().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "captured account is unavailable",
                )
            })?;
            validate_principal(
                initial.as_ref().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "captured account is unavailable",
                    )
                })?,
                &auth,
            )?;
            Some(RefreshSource::Account(AccountSource {
                credentials: owner,
                issuer: Arc::downgrade(self),
            }))
        } else {
            None
        };
        if *self.auth_change_state_receiver().borrow() != revision {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "credential source changed during request admission",
            ));
        }
        self.capture_source(|auth| {
            Ok(Arc::new(match (auth, refresh) {
                (Some(auth), Some(refresh)) => Source::Request {
                    initial: auth.clone(),
                    current: std::sync::RwLock::new(auth.clone()),
                    refresh,
                },
                _ => Source::Static(auth.clone()),
            }))
        })
    }

    pub(in crate::auth::manager) fn reject_captured_resolution(&self, error: &RefreshTokenError) {
        if let RefreshTokenError::Permanent(error) = error
            && let Some(auth) = self.auth_cached()
        {
            self.record_permanent_refresh_failure_if_unchanged(&auth, error);
        }
    }

    fn capture_source(
        &self,
        source: impl FnOnce(&Option<CodexAuth>) -> io::Result<Arc<Source>>,
    ) -> io::Result<CapturedAuthSource> {
        let revision = *self.auth_change_state_receiver().borrow();
        let auth = self.auth_cached();
        if let Some(failure) = auth
            .as_ref()
            .and_then(|auth| self.refresh_failure_for_auth(auth))
        {
            return Err(io::Error::other(failure));
        }
        let source = source(&auth)?;
        let mut manager = Self::from_auth_state(self.saved_account_config(), false, auth);
        manager.allowed_auth_modes = std::sync::RwLock::new(
            self.allowed_auth_modes
                .read()
                .map_err(|_| io::Error::other("credential policy is unavailable"))?
                .clone(),
        );
        manager.credential_origin = std::sync::RwLock::new(self.credential_origin());
        manager.auth_change_state_tx.send_replace(revision);
        manager.auth_publication = AuthPublication::Captured {
            policy: self.http_client_factory().network_policy().clone(),
            requirement: if matches!(source.as_ref(), Source::Static(None)) {
                SourceRequirement::Anonymous
            } else {
                SourceRequirement::Required
            },
        };
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
        let principal = Principal::capture(&manager);
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
        let principal = Principal::capture(self);
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
            account_routes: super::AccountRoutes::default(),
            inner: RwLock::new(CachedAuth {
                account: super::AccountSelection::Login,
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
            unsettled_chatgpt_refresh: Mutex::new(None),
            allowed_auth_modes: RwLock::new(None),
            agent_identity_lock: Semaphore::new(1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(None),
            auth_publication: AuthPublication::Owner,
            credential_origin: RwLock::new(Arc::new(())),
            workload_identity_selected: false,
            auth_route_config,
        }
    }
}
