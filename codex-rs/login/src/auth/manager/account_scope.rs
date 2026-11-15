#[path = "captured_source.rs"]
mod captured_source;

pub(super) use captured_source::AuthPublication;
pub use captured_source::CapturedAuthSource;
pub(super) use captured_source::SourceRequirement;

use super::Arc;
use super::AuthConfig;
use super::AuthCredentialsStoreMode;
use super::AuthManager;
use super::CodexAuth;
use super::agent_identity_authapi_base_url;
use super::create_auth_storage;
use super::validate_auth_restrictions;
use crate::auth::SavedAccount;
use crate::auth::accounts::identity;

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) enum AccountSelection {
    #[default]
    Login,
    Selected(Arc<AccountRoute>),
}

#[derive(PartialEq, Eq)]
pub(super) struct AccountRoute {
    id: String,
    routes: crate::AuthRouteConfig,
}

impl AccountSelection {
    pub(super) fn id(&self) -> Option<&str> {
        match self {
            Self::Login => None,
            Self::Selected(route) => Some(&route.id),
        }
    }

    pub(super) fn routes<'a>(
        &'a self,
        login: &'a crate::AuthRouteConfig,
    ) -> &'a crate::AuthRouteConfig {
        match self {
            Self::Login => login,
            Self::Selected(route) => &route.routes,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct AccountRoutes(
    Arc<std::sync::Mutex<std::collections::HashMap<String, codex_http_client::WeakNetworkPolicy>>>,
);

impl AccountRoutes {
    fn select(
        &self,
        id: &str,
        routes: &crate::AuthRouteConfig,
    ) -> std::io::Result<Arc<AccountRoute>> {
        let mut scopes = self
            .0
            .lock()
            .map_err(|_| std::io::Error::other("account policy registry is unavailable"))?;
        // Factories retain the scope, not this route wrapper. A weak route
        // would forget a still-live account when the selector moves away.
        let retained = scopes
            .get(id)
            .and_then(codex_http_client::WeakNetworkPolicy::upgrade);
        scopes.retain(|_, policy| policy.upgrade().is_some());
        let route = Arc::new(AccountRoute {
            id: id.to_owned(),
            routes: routes.for_personal_account(retained)?,
        });
        scopes.insert(
            id.to_owned(),
            route.routes.application_network_policy().downgrade(),
        );
        Ok(route)
    }
}

pub(super) enum AuthTransition {
    Credential,
    Selection,
}

// Failure to observe the store is not evidence that the selected identity was removed.
pub(super) enum SavedAccountLoadError {
    Storage(std::io::Error),
    Rejected(std::io::Error),
}

impl From<SavedAccountLoadError> for std::io::Error {
    fn from(error: SavedAccountLoadError) -> Self {
        match error {
            SavedAccountLoadError::Storage(error) | SavedAccountLoadError::Rejected(error) => error,
        }
    }
}

/// Validated selection, held until its caller has durably recorded the conversation binding.
pub struct PreparedAccount<'a> {
    manager: &'a AuthManager,
    route: Arc<AccountRoute>,
    auth: CodexAuth,
    _refresh: tokio::sync::SemaphorePermit<'a>,
}

impl PreparedAccount<'_> {
    pub fn commit(self) {
        let mut cache = self
            .manager
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.account = AccountSelection::Selected(self.route);
        self.manager
            .set_cached_auth_locked(&mut cache, Some(self.auth), AuthTransition::Selection);
    }
}

impl AuthManager {
    pub(super) fn account_selection(&self) -> AccountSelection {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .account
            .clone()
    }

    pub(super) fn current_auth_routes(&self) -> crate::AuthRouteConfig {
        self.account_selection()
            .routes(&self.auth_route_config)
            .clone()
    }

    pub fn is_account_scoped(&self) -> bool {
        matches!(self.account_selection(), AccountSelection::Selected(_))
    }
    pub fn saved_accounts(&self) -> std::io::Result<Vec<SavedAccount>> {
        Ok(create_auth_storage(
            self.codex_home.clone(),
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )
        .read()?
        .map_or_else(Vec::new, super::super::accounts::AuthDocument::accounts))
    }

    pub fn selected_account_id(&self) -> Option<String> {
        if self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
            return None;
        }
        self.account_selection()
            .id()
            .map(str::to_owned)
            .or_else(|| {
                let auth = self.auth_cached()?;
                if auth.is_workspace_account() {
                    return None;
                }
                match auth {
                    CodexAuth::Chatgpt(auth) if !auth.storage().is_ephemeral() => {
                        auth.current_auth_json().as_ref().and_then(identity)
                    }
                    _ => None,
                }
            })
    }

    /// The active login's personal account, read from storage rather than any manager's cache or
    /// scope: the account last chosen with `/account`, or the first one logged in.
    pub fn active_account_id(&self) -> std::io::Result<Option<String>> {
        // TELOS(pinned-accounts): Managed workspace auth cannot select personal accounts;
        // do not block thread creation on a store whose result would be discarded.
        if self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral
            || self
                .auth_cached()
                .is_some_and(|auth| auth.is_workspace_account())
        {
            return Ok(None);
        }
        Ok(create_auth_storage(
            self.codex_home.clone(),
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )
        .read()?
        .map(|document| document.active)
        .filter(|active| {
            !active
                .tokens
                .as_ref()
                .is_some_and(|tokens| tokens.id_token.is_workspace_account())
        })
        .as_ref()
        .and_then(identity))
    }

    /// Makes a saved account the active login. Logging in never moves it; existing conversations
    /// keep their own bindings.
    pub async fn activate_account(&self, id: &str) -> std::io::Result<()> {
        if self.workload_identity_selected
            || self.has_external_auth()
            || self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral
        {
            return Err(std::io::Error::other(
                "Host-managed authentication cannot select a saved account.",
            ));
        }
        // Login-method and workspace restrictions bind the active login as they bind a conversation.
        self.load_saved_account(id).await?;
        crate::auth::activate_account(
            &self.codex_home,
            id,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )?;
        self.reload().await;
        Ok(())
    }

    /// A conversation owns its selector; selecting an account never retargets another conversation.
    pub async fn for_account(&self, id: &str) -> std::io::Result<Arc<Self>> {
        if self.workload_identity_selected
            || self.has_external_auth()
            || self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral
        {
            return Err(std::io::Error::other(
                "Host-managed authentication cannot select a saved account.",
            ));
        }
        let auth = match self.load_saved_account(id).await {
            Ok(auth) => Some(auth),
            // Retain an unavailable binding without silently spending another login's quota.
            Err(SavedAccountLoadError::Rejected(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            Err(error) => return Err(error.into()),
        };
        self.ensure_personal_account_selection(auth.as_ref())?;
        let route = self.account_routes.select(id, &self.auth_route_config)?;
        let mut config = self.saved_account_config();
        config.auth_route_config = route.routes.clone();
        let mut manager =
            Self::from_auth_state(config, /*enable_codex_api_key_env*/ false, auth);
        manager.account_routes = self.account_routes.clone();
        manager.credential_origin = std::sync::RwLock::new(self.credential_origin());
        manager.allowed_auth_modes = std::sync::RwLock::new(
            self.allowed_auth_modes
                .read()
                .map_err(|_| std::io::Error::other("credential policy is unavailable"))?
                .clone(),
        );
        if let Some(resolver) = self.workspace_routing_resolver.get() {
            manager.set_workspace_routing_resolver(resolver.clone());
        }
        manager
            .inner
            .get_mut()
            .map_err(|_| std::io::Error::other("credential cache is unavailable"))?
            .account = AccountSelection::Selected(route);
        Ok(Arc::new(manager))
    }

    pub(super) fn frozen_account_owner(&self) -> std::io::Result<Arc<Self>> {
        let cache = self
            .inner
            .read()
            .map_err(|_| std::io::Error::other("credential cache is unavailable"))?
            .clone();
        let id = self.selected_account_id();
        let workspace = cache
            .auth
            .as_ref()
            .is_some_and(CodexAuth::is_workspace_account);
        if id.is_none() && !workspace {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "saved account is unavailable",
            ));
        }
        let routes = cache.account.routes(&self.auth_route_config).clone();
        let mut config = self.saved_account_config();
        config.auth_route_config = routes.clone();
        if workspace {
            config.forced_chatgpt_workspace_id = Some(vec![
                cache
                    .auth
                    .as_ref()
                    .and_then(CodexAuth::get_account_id)
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "workspace is unavailable",
                        )
                    })?,
            ]);
        }
        let mut manager = Self::from_auth_state(config, false, cache.auth.clone());
        manager.account_routes = self.account_routes.clone();
        manager
            .inner
            .get_mut()
            .map_err(|_| std::io::Error::other("credential cache is unavailable"))?
            .account = match cache.account {
            AccountSelection::Selected(route) => AccountSelection::Selected(route),
            AccountSelection::Login => match id {
                Some(id) => AccountSelection::Selected(Arc::new(AccountRoute { id, routes })),
                None => AccountSelection::Login,
            },
        };
        manager.refresh_lock = Arc::clone(&self.refresh_lock);
        manager.credential_origin = std::sync::RwLock::new(self.credential_origin());
        manager.allowed_auth_modes = std::sync::RwLock::new(
            self.allowed_auth_modes
                .read()
                .map_err(|_| std::io::Error::other("credential policy is unavailable"))?
                .clone(),
        );
        manager
            .auth_change_state_tx
            .send_replace(*self.auth_change_state_receiver().borrow());
        if let Some(resolver) = self.workspace_routing_resolver.get() {
            manager.set_workspace_routing_resolver(resolver.clone());
        }
        Ok(Arc::new(manager))
    }

    /// Callers must serialize this with their conversation's turn admission.
    pub async fn select_account(&self, id: &str) -> std::io::Result<()> {
        self.prepare_account(id).await?.commit();
        Ok(())
    }

    pub async fn prepare_account(&self, id: &str) -> std::io::Result<PreparedAccount<'_>> {
        if self.workload_identity_selected
            || self.has_external_auth()
            || self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral
        {
            return Err(std::io::Error::other(
                "Host-managed authentication cannot select a saved account.",
            ));
        }
        let refresh = self
            .refresh_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        let auth = self.load_saved_account(id).await?;
        self.ensure_personal_account_selection(Some(&auth))?;
        Ok(PreparedAccount {
            manager: self,
            route: self.account_routes.select(id, &self.auth_route_config)?,
            auth,
            _refresh: refresh,
        })
    }

    fn ensure_personal_account_selection(&self, auth: Option<&CodexAuth>) -> std::io::Result<()> {
        if auth.is_some_and(CodexAuth::is_workspace_account)
            || self
                .auth_cached()
                .is_some_and(|current| current.is_workspace_account())
        {
            return Err(std::io::Error::other(
                "Per-conversation switching supports personal accounts only; managed workspace policies are server-scoped.",
            ));
        }
        Ok(())
    }

    pub(super) async fn load_saved_account(
        &self,
        id: &str,
    ) -> Result<CodexAuth, SavedAccountLoadError> {
        let cached = self.auth_cached();
        let auth = self
            .saved_account_config()
            .read_saved_account(id, cached.as_ref())
            .await?;
        let modes = self.allowed_auth_modes.read().map_err(|_| {
            SavedAccountLoadError::Storage(std::io::Error::other(
                "credential policy is unavailable",
            ))
        })?;
        if modes
            .as_ref()
            .is_some_and(|modes| !modes.contains(&auth.api_auth_mode()))
        {
            return Err(SavedAccountLoadError::Rejected(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "selected account authentication mode is not allowed",
            )));
        }
        Ok(auth)
    }

    fn saved_account_config(&self) -> AuthConfig {
        AuthConfig {
            codex_home: self.codex_home.clone(),
            auth_credentials_store_mode: self.auth_credentials_store_mode,
            keyring_backend_kind: self.keyring_backend_kind,
            forced_login_method: self.forced_login_method,
            chatgpt_base_url: self.chatgpt_base_url.clone(),
            forced_chatgpt_workspace_id: self.effective_chatgpt_workspaces(),
            managed_auth_policy: self.managed_auth_policy.clone(),
            auth_route_config: self.current_auth_routes(),
        }
    }
}

impl AuthConfig {
    /// Reads one saved identity without changing the selected account or refreshing tokens.
    pub async fn load_saved_account(&self, id: &str) -> std::io::Result<CodexAuth> {
        self.read_saved_account(id, None).await.map_err(Into::into)
    }

    async fn read_saved_account(
        &self,
        id: &str,
        cached_auth: Option<&CodexAuth>,
    ) -> Result<CodexAuth, SavedAccountLoadError> {
        let backend = create_auth_storage(
            self.codex_home.clone(),
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        );
        let document = super::load_stored_auth(Arc::new(crate::auth::accounts::AccountStorage {
            backend,
            id: id.to_owned(),
        }))
        .await
        .map_err(SavedAccountLoadError::Storage)?
        .ok_or_else(|| {
            SavedAccountLoadError::Rejected(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Saved account is unavailable; use /account or codex login.",
            ))
        })?;
        let auth = CodexAuth::from_auth_dot_json(
            &self.codex_home,
            document,
            self.auth_credentials_store_mode,
            self.chatgpt_base_url.as_deref(),
            self.keyring_backend_kind,
            agent_identity_authapi_base_url(self.chatgpt_base_url.as_deref())
                .ok()
                .as_deref(),
            &self.auth_route_config,
            cached_auth,
        )
        .await
        .map_err(SavedAccountLoadError::Rejected)?;
        validate_auth_restrictions(
            Some(&self.allowed_login_methods()),
            self.effective_chatgpt_workspaces().as_deref(),
            &auth,
        )
        .map_err(|error| SavedAccountLoadError::Rejected(std::io::Error::other(error)))?;
        Ok(auth)
    }
}
