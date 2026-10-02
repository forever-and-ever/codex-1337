//! Native subscription account inventory; credentials remain inside the login registry.

use anyhow::Result;
use anyhow::bail;
use codex_app_server_protocol::ClaudeSubscriptionAccount;
use codex_app_server_protocol::SubscriptionAccountProvider;
use codex_core::config::Config;
use codex_login::anthropic::AccountId;
use codex_login::anthropic::AccountStore;

pub(super) struct ClaudeAccountController {
    store: AccountStore,
}

impl ClaudeAccountController {
    pub fn new(config: &Config) -> Self {
        Self {
            store: AccountStore::new(config.codex_home.to_path_buf()),
        }
    }

    pub fn list(&self, config: &Config) -> Result<Vec<ClaudeSubscriptionAccount>> {
        Ok(self
            .store
            .list()?
            .into_iter()
            .map(|account| {
                let id = account.identity.id().to_string();
                ClaudeSubscriptionAccount {
                    nickname: config
                        .wrangler
                        .account_nicknames
                        .get(SubscriptionAccountProvider::ClaudeSubscription.config_id())
                        .and_then(|names| names.get(&id))
                        .cloned(),
                    id,
                    email: account.email,
                    available: true,
                    status: None,
                }
            })
            .collect())
    }

    pub fn validate_selection(&self, id: &str) -> Result<AccountId> {
        let id: AccountId = id.parse()?;
        if !self
            .store
            .list()?
            .iter()
            .any(|account| account.identity.id() == id)
        {
            bail!("Claude subscription account is not saved");
        }
        Ok(id)
    }
}
