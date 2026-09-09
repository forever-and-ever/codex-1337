use super::*;
use crate::auth::SavedAccount;
use crate::auth::accounts::identity;

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
    id: String,
    auth: CodexAuth,
    _refresh: tokio::sync::SemaphorePermit<'a>,
}

impl PreparedAccount<'_> {
    pub fn commit(self) {
        let mut scope = self.manager.write_account_scope();
        *scope = Some(self.id);
        self.manager.set_cached_auth(Some(self.auth));
    }
}

impl AuthManager {
    #[expect(
        clippy::expect_used,
        reason = "A poisoned selector cannot safely choose credentials."
    )]
    pub(super) fn read_account_scope(&self) -> std::sync::RwLockReadGuard<'_, Option<String>> {
        self.account_scope
            .read()
            .expect("account selector lock poisoned")
    }

    #[expect(
        clippy::expect_used,
        reason = "A poisoned selector cannot safely choose credentials."
    )]
    pub(super) fn write_account_scope(&self) -> std::sync::RwLockWriteGuard<'_, Option<String>> {
        self.account_scope
            .write()
            .expect("account selector lock poisoned")
    }

    pub fn is_account_scoped(&self) -> bool {
        self.read_account_scope().is_some()
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
        self.read_account_scope().clone().or_else(|| {
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
        let manager = Self::new_from_auth_config(
            AuthConfig {
                codex_home: self.codex_home.clone(),
                auth_credentials_store_mode: self.auth_credentials_store_mode,
                keyring_backend_kind: self.keyring_backend_kind,
                forced_login_method: self.forced_login_method,
                chatgpt_base_url: self.chatgpt_base_url.clone(),
                forced_chatgpt_workspace_id: self.effective_chatgpt_workspaces(),
                managed_auth_policy: self.managed_auth_policy.clone(),
                auth_route_config: self.auth_route_config.clone(),
            },
            /*enable_codex_api_key_env*/ false,
        )
        .await;
        if let Err(error) = manager.select_account(id).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error);
            }
            // Retain an unavailable binding so the TUI can offer /account without
            // silently spending another login's quota while resuming this thread.
            let mut scope = manager.write_account_scope();
            *scope = Some(id.to_owned());
            manager.set_cached_auth(None);
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
        if self.selected_account_id().as_deref() != Some(id)
            && (auth.is_workspace_account()
                || self
                    .auth_cached()
                    .is_some_and(|current| current.is_workspace_account()))
        {
            return Err(std::io::Error::other(
                "Per-conversation switching supports personal accounts only; managed workspace policies are server-scoped.",
            ));
        }
        Ok(PreparedAccount {
            manager: self,
            id: id.to_owned(),
            auth,
            _refresh: refresh,
        })
    }

    pub(super) async fn load_saved_account(
        &self,
        id: &str,
    ) -> Result<CodexAuth, SavedAccountLoadError> {
        let document = create_auth_storage(
            self.codex_home.clone(),
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )
        .load_account(id)
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
            self.agent_identity_authapi_base_url.as_deref(),
            &self.auth_route_config,
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
