//! Captured request admission and the model client's application-template boundary.

use std::sync::Arc;

use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_runtime_agent_instance::RuntimeAuthBinding;
use codex_runtime_agent_instance::RuntimeAuthManager;

use crate::BearerAuthProvider;
use crate::ModelProvider;
use crate::ProviderAuthScope;
use crate::ProviderCredentialGuard;
use crate::WorkspaceRoutingContext;
use crate::auth::bearer_auth_for_provider;
use crate::request_setup::ResolvedProviderRequest;
use crate::request_setup::resolve_request;

pub(super) async fn admit_runtime(
    info: &ModelProviderInfo,
    manager: Option<&Arc<AuthManager>>,
    runtime: &Arc<RuntimeAuthManager>,
    guard: Option<&dyn ProviderCredentialGuard>,
) -> Result<Arc<RuntimeAuthBinding>> {
    let runtime_manager = Arc::clone(runtime);
    let runtime = if guard.is_some()
        && (info.env_key.is_some() || info.experimental_bearer_token.is_some())
    {
        runtime.bind(manager.cloned().ok_or_else(|| {
            CodexErr::InvalidRequest("captured provider bearer is unavailable".into())
        })?)
    } else {
        crate::select_runtime_provider(info, manager, runtime)?
    };
    runtime_manager
        .admit_binding(&runtime)
        .await
        .map_err(crate::runtime_auth::map_runtime_auth_error)
}

pub(super) async fn resolve_bearer(
    info: &ModelProviderInfo,
    manager: Option<&Arc<AuthManager>>,
    guard: Option<&dyn ProviderCredentialGuard>,
) -> Result<Option<BearerAuthProvider>> {
    if guard.is_some() && (info.env_key.is_some() || info.experimental_bearer_token.is_some()) {
        let auth = match guard.and_then(ProviderCredentialGuard::configured_bearer) {
            Some(auth) => Some(auth),
            None => match manager {
                Some(manager) => manager.auth().await,
                None => None,
            },
        };
        let token = auth.as_ref().and_then(CodexAuth::api_key).ok_or_else(|| {
            CodexErr::InvalidRequest("captured provider bearer is unavailable".into())
        })?;
        Ok(Some(BearerAuthProvider::new(token.to_owned())))
    } else {
        bearer_auth_for_provider(info)
    }
}

/// The model client's Config factory is a template, not a prior account admission.
/// Personal-account requests use the live route's original local transport and exact
/// account scope. Other callers keep `resolve_request`'s strict captured-factory contract.
pub async fn resolve_model_client_request(
    provider: &dyn ModelProvider,
    scope: Option<ProviderAuthScope>,
    routing: Option<&WorkspaceRoutingContext>,
    template: HttpClientFactory,
) -> Result<ResolvedProviderRequest> {
    let factory = provider
        .auth_manager()
        .map(|manager| manager.http_client_factory())
        .filter(|factory| factory.network_policy().has_account_scope())
        .map(|factory| {
            let policy = factory
                .network_policy()
                .clone()
                .restrict_to_policy_endpoints(template.network_policy());
            factory.with_network_policy(policy)
        })
        .unwrap_or(template);
    resolve_request(provider, scope, routing, factory).await
}
