use std::fmt;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

// TELOS(recoverable-sign-in): one provider identity across accounts and auth failures.
#[derive(
    Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, Hash, JsonSchema, TS,
)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum AccountProvider {
    #[default]
    Openai,
    ClaudeSubscription,
    #[serde(rename = "openrouter")]
    #[ts(rename = "openrouter")]
    OpenRouter,
}

/// The saved identity whose grant no longer permits requests.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SignInRequired {
    pub provider: AccountProvider,
    pub account: String,
}

impl SignInRequired {
    pub fn provider_name(&self) -> &'static str {
        match self.provider {
            AccountProvider::Openai => "ChatGPT",
            AccountProvider::ClaudeSubscription => "Claude",
            AccountProvider::OpenRouter => "OpenRouter",
        }
    }

    pub fn recovery_hint(&self) -> &'static str {
        match self.provider {
            AccountProvider::Openai => "codex login --provider openai",
            AccountProvider::ClaudeSubscription => "codex login --provider claude_subscription",
            AccountProvider::OpenRouter => "/account",
        }
    }
}

impl fmt::Display for SignInRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} sign-in expired for {}. Sign in again with `{}`.",
            self.provider_name(),
            self.account,
            self.recovery_hint(),
        )
    }
}

impl AccountProvider {
    pub fn config_id(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::ClaudeSubscription => "claude_subscription",
            Self::OpenRouter => "openrouter",
        }
    }
}
