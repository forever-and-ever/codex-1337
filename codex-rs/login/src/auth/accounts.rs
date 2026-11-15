//! Multiple `ChatGPT` logins in the existing credential-store document.
//! The flattened active record retains the upstream single-login representation.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use codex_config::types::AuthCredentialsStoreMode;
use serde::Deserialize;
use serde::Serialize;

use super::storage::AuthDotJson;
use super::storage::AuthKeyringBackendKind;
use super::storage::AuthStorageBackend;
use super::storage::create_auth_storage;
use super::storage::credential_lock;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct AuthDocument {
    #[serde(flatten)]
    pub active: AuthDotJson,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub wrangler_accounts: BTreeMap<String, AuthDotJson>,
}

/// Account-list projection without credential material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAccount {
    pub id: String,
    pub email: Option<String>,
    pub plan: Option<String>,
}

pub(super) fn identity(auth: &AuthDotJson) -> Option<String> {
    if auth.resolved_mode() != codex_protocol::auth::AuthMode::Chatgpt {
        return None;
    }
    let tokens = auth.tokens.as_ref()?;
    let account = tokens
        .account_id
        .as_ref()
        .or(tokens.id_token.chatgpt_account_id.as_ref())?;
    let user = tokens.id_token.chatgpt_user_id.as_deref().unwrap_or("");
    Some(format!("{user}:{account}"))
}

/// Persists an interactive login: it joins the saved accounts without displacing an active one.
pub fn admit_auth(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let backend = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    if backend.is_ephemeral() {
        return backend.save(auth);
    }
    let _guard = credential_lock(backend.home())?;
    let mut document = backend
        .read()?
        .unwrap_or_else(|| AuthDocument::new(auth.clone()));
    document.admit(auth.clone());
    backend.write(&document)
}

/// Makes a saved account the active login: the account fresh conversations and server-wide
/// work use until another is chosen.
pub fn activate_account(
    codex_home: &Path,
    id: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let backend = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let _guard = credential_lock(backend.home())?;
    let unavailable = || {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Saved account is unavailable; use /account or codex login.",
        )
    };
    let mut document = backend.read()?.ok_or_else(unavailable)?;
    if !document.activate(id) {
        return Err(unavailable());
    }
    backend.write(&document)
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;

impl AuthDocument {
    pub fn new(active: AuthDotJson) -> Self {
        Self {
            active,
            wrangler_accounts: BTreeMap::new(),
        }
    }

    pub fn retain_active(&mut self) {
        if let Some(id) = identity(&self.active) {
            self.wrangler_accounts.insert(id, self.active.clone());
        }
    }

    pub fn select(&self, id: &str) -> Option<AuthDotJson> {
        if identity(&self.active).as_deref() == Some(id) {
            Some(self.active.clone())
        } else {
            self.wrangler_accounts
                .get(id)
                .filter(|auth| identity(auth).as_deref() == Some(id))
                .cloned()
        }
    }

    /// Makes a login active; the displaced login stays saved.
    pub fn install(&mut self, auth: AuthDotJson) {
        self.retain_active();
        if let Some(id) = identity(&auth) {
            self.wrangler_accounts.remove(&id);
        }
        self.active = auth;
    }

    /// A login joins the saved accounts. It displaces the active login only when it renews that
    /// account, when no `ChatGPT` account is active, or when it has no identity to be saved under;
    /// choosing among saved accounts belongs to `/account`.
    pub fn admit(&mut self, auth: AuthDotJson) {
        if let Some(id) = identity(&auth)
            && identity(&self.active).is_some_and(|active| active != id)
        {
            self.wrangler_accounts.insert(id, auth);
            return;
        }
        self.install(auth);
    }

    /// Makes a saved account active; the displaced login stays saved.
    pub fn activate(&mut self, id: &str) -> bool {
        let Some(auth) = self.select(id) else {
            return false;
        };
        self.install(auth);
        true
    }

    pub fn accounts(mut self) -> Vec<SavedAccount> {
        self.retain_active();
        self.wrangler_accounts
            .into_iter()
            .filter_map(|(id, auth)| {
                if identity(&auth).as_deref() != Some(&id) {
                    return None;
                }
                let tokens = auth.tokens?;
                Some(SavedAccount {
                    id,
                    email: tokens.id_token.email.clone(),
                    plan: tokens.id_token.get_chatgpt_plan_type_raw(),
                })
            })
            .collect()
    }
}

#[derive(Debug)]
pub(super) struct AccountStorage {
    pub backend: Arc<dyn AuthStorageBackend>,
    pub id: String,
}

impl AuthStorageBackend for AccountStorage {
    fn is_ephemeral(&self) -> bool {
        self.backend.is_ephemeral()
    }

    fn home(&self) -> &Path {
        self.backend.home()
    }

    fn read(&self) -> std::io::Result<Option<AuthDocument>> {
        self.backend.read()
    }

    fn write(&self, document: &AuthDocument) -> std::io::Result<()> {
        self.backend.write(document)
    }

    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        self.backend.load_account(&self.id)
    }

    fn compare_exchange(
        &self,
        previous: &AuthDotJson,
        updated: &AuthDotJson,
    ) -> std::io::Result<bool> {
        if identity(previous).as_deref() != Some(&self.id)
            || identity(updated).as_deref() != Some(&self.id)
        {
            return Err(std::io::Error::other(
                "Token refresh changed the pinned account identity.",
            ));
        }
        // TELOS(credential-backend-authority): preserve the backend's atomic source choice.
        self.backend.compare_exchange(previous, updated)
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        if identity(auth).as_deref() != Some(&self.id) {
            return Err(std::io::Error::other(
                "Refusing to replace an account with another identity.",
            ));
        }
        let _guard = credential_lock(self.home())?;
        let mut document = self
            .read()?
            .filter(|document| document.select(&self.id).is_some())
            .ok_or_else(|| std::io::Error::other("This account has been removed; log in again."))?;
        if identity(&document.active).as_deref() == Some(&self.id) {
            document.active = auth.clone();
            document.wrangler_accounts.remove(&self.id);
        } else {
            document
                .wrangler_accounts
                .insert(self.id.clone(), auth.clone());
        }
        self.write(&document)
    }

    fn delete(&self) -> std::io::Result<bool> {
        if self.load()?.is_none() {
            return Ok(false);
        }
        let _guard = credential_lock(self.home())?;
        let Some(mut document) = self.read()? else {
            return Ok(false);
        };
        document.retain_active();
        let removed = document.wrangler_accounts.remove(&self.id).is_some();
        if identity(&document.active).as_deref() == Some(&self.id) {
            let Some((_, replacement)) = document.wrangler_accounts.pop_first() else {
                return self.backend.delete_unlocked();
            };
            document.active = replacement;
        }
        if let Some(active) = identity(&document.active) {
            document.wrangler_accounts.remove(&active);
        }
        self.write(&document)?;
        Ok(removed)
    }

    fn delete_unlocked(&self) -> std::io::Result<bool> {
        self.delete()
    }

    fn load_account_grant(&self) -> std::io::Result<Option<AuthDotJson>> {
        Ok(self
            .backend
            .read_account_grant()?
            .and_then(|document| document.select(&self.id)))
    }
}
