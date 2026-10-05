mod anthropic_auth;
mod anthropic_quota;
mod auth;
mod bearer_auth_provider;
mod claude_catalog;
mod combined_auth;
mod models_endpoint;
mod models_identity;
mod native_claude_catalog;
mod provider;
mod shared_state;
pub mod test_support;
mod workspace_routing;
pub use workspace_routing::ACCOUNT_ROUTING_HEADER;
pub use workspace_routing::ResolvedResponsesProvider;
pub use workspace_routing::ResponsesConnectionKey;
pub use workspace_routing::WorkspaceRoutingContext;

pub use anthropic_auth::AnthropicAuthContext;
pub use anthropic_auth::AnthropicRequestAuth;
pub use anthropic_quota::AnthropicQuota;
pub use anthropic_quota::AnthropicQuotaError;
pub use anthropic_quota::read_anthropic_quota;
pub use auth::AgentIdentitySessionFallback;
pub use auth::ProviderAuthScope;
pub use auth::ResolvedProviderAuth;
pub use auth::auth_provider_from_auth;
pub use auth::auth_provider_from_auth_manager;
pub use auth::unauthenticated_auth_provider;
pub use bearer_auth_provider::BearerAuthProvider;
pub use bearer_auth_provider::BearerAuthProvider as CoreAuthProvider;
pub use codex_model_provider_info::CHATGPT_CODEX_BASE_URL;
pub use codex_protocol::account::ProviderAccount;
pub use provider::ModelProvider;
pub use provider::ModelProviderFuture;
pub use provider::ProviderAccountError;
pub use provider::ProviderAccountResult;
pub use provider::ProviderAccountState;
pub use provider::ProviderAuthRecoveryMessages;
pub use provider::ProviderCapabilities;
pub use provider::ProviderUnauthorizedRecovery;
pub use provider::RemoteCompactionSupport;
pub use provider::SharedModelProvider;
pub use provider::create_model_provider;
pub use provider::create_model_provider_with_anthropic_account;

#[cfg(test)]
#[path = "workspace_routing_tests.rs"]
mod workspace_routing_tests;
