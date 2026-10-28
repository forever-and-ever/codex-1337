use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::Weak;

use codex_keyring_store::DefaultKeyringStore;
use codex_login::AuthRuntimeConfig;
use codex_login::GatewayAuthConfig;
use codex_login::GatewayAuthManager;
use codex_model_provider_info::GatewayOAuthConfig;

/// Provider-owned runtime state shared across independently configured sessions.
#[derive(Default)]
pub(crate) struct ModelProviderSharedState {
    // Retain pending rotated grants even after the last session or catalogue request drops.
    anthropic_managers: Mutex<Vec<(AuthRuntimeConfig, Arc<codex_login::anthropic::AuthManager>)>>,
    gateway_managers: Mutex<
        Vec<(
            GatewayAuthConfig,
            AuthRuntimeConfig,
            Weak<GatewayAuthManager>,
        )>,
    >,
}

pub(crate) fn process_shared_state() -> &'static ModelProviderSharedState {
    static STATE: OnceLock<ModelProviderSharedState> = OnceLock::new();
    STATE.get_or_init(ModelProviderSharedState::default)
}

impl ModelProviderSharedState {
    pub(crate) fn anthropic_auth(
        &self,
        runtime: &AuthRuntimeConfig,
    ) -> std::io::Result<Arc<codex_login::anthropic::AuthManager>> {
        let mut managers = self
            .anthropic_managers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(manager) = managers
            .iter()
            .find(|(cached_runtime, _)| cached_runtime == runtime)
            .map(|(_, manager)| Arc::clone(manager))
        {
            return Ok(manager);
        }
        let manager = Arc::new(codex_login::anthropic::AuthManager::new(
            codex_login::anthropic::AccountStore::new(runtime.codex_home.clone()),
            runtime.auth_route_config.http_client_factory(),
        )?);
        managers.push((runtime.clone(), Arc::clone(&manager)));
        Ok(manager)
    }

    pub(crate) fn gateway_auth(
        &self,
        config: &GatewayOAuthConfig,
        runtime: &AuthRuntimeConfig,
    ) -> std::io::Result<Arc<GatewayAuthManager>> {
        let oauth = GatewayAuthConfig {
            authorization_url: config.authorization_url.clone(),
            device_authorization_url: None,
            token_url: config.token_url.clone(),
            client_id: config.client_id.clone(),
            resource: config.resource.clone(),
            scopes: config.scopes.clone(),
            redirect_port: config.redirect_port,
        };
        let mut managers = self
            .gateway_managers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        managers.retain(|(_, _, manager)| manager.strong_count() != 0);
        if let Some(manager) = managers
            .iter()
            .find(|(cached_config, cached_runtime, _)| {
                cached_config == &oauth && cached_runtime == runtime
            })
            .and_then(|(_, _, manager)| manager.upgrade())
        {
            return Ok(manager);
        }
        let manager = Arc::new(GatewayAuthManager::new(
            oauth.clone(),
            runtime.codex_home.clone(),
            runtime.auth_route_config.http_client_factory(),
            Arc::new(DefaultKeyringStore),
        )?);
        managers.push((oauth, runtime.clone(), Arc::downgrade(&manager)));
        Ok(manager)
    }
}

#[cfg(test)]
#[path = "shared_state_tests.rs"]
mod tests;

#[path = "shared_state_test_support.rs"]
pub(crate) mod test_support;
