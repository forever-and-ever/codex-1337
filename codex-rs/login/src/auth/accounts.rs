//! Multiple ChatGPT logins in the existing credential-store document.
//! The flattened active record retains the upstream single-login representation.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;

use super::storage::AuthDotJson;
use super::storage::AuthStorageBackend;
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
    if auth
        .auth_mode
        .is_some_and(|mode| mode != codex_protocol::auth::AuthMode::Chatgpt)
        || auth.openai_api_key.is_some()
    {
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
        Ok(self.read()?.and_then(|document| document.select(&self.id)))
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
                return self.backend.delete();
            };
            document.active = replacement;
        }
        if let Some(active) = identity(&document.active) {
            document.wrangler_accounts.remove(&active);
        }
        self.write(&document)?;
        Ok(removed)
    }
}
