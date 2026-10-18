//! Subscription identities and account-local credentials, independent of `OpenAI` authentication.

mod callback;
mod identity;
mod login;
mod manager;
mod storage;
mod token;

pub use identity::AccountId;
pub use identity::AccountIdentity;
pub use identity::AccountInfo;
pub use login::LoginAttempt;
pub use login::LoginCancelHandle;
pub use manager::Account;
pub use manager::AuthError;
pub use manager::AuthManager;
pub use storage::AccountStore;
pub use token::Credentials;
pub use token::SubscriptionGrant;

pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const OAUTH_SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
