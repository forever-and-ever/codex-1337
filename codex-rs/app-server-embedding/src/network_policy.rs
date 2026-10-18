//! Network policy that an embedder shares with the app-server it starts in-process.

use codex_config::ApplicationRequirementsToml;
use codex_config::NetworkDomainPermissionToml;
use codex_core::config::Config;
use codex_http_client::DestinationPolicy;

/// Policy shared by transports created before and owned by the embedded app-server.
#[derive(Clone, Default)]
pub struct EmbeddedNetworkPolicy {
    /// Policy for all traffic, published once the effective requirements load.
    pub effective: codex_http_client::NetworkPolicyController,
    /// Policy from local requirements alone, which cloud bootstrap uses.
    pub local: codex_http_client::NetworkPolicyController,
}

impl EmbeddedNetworkPolicy {
    /// Loads the local rules for cloud bootstrap; all other traffic starts unavailable.
    pub async fn load(overrides: &codex_config::LoaderOverrides) -> Self {
        let policy = Self::default();
        let local = async {
            codex_config::loader::load_local_application_requirements(
                codex_exec_server::LOCAL_FS.as_ref(),
                overrides,
            )
            .await?
            .compose(codex_config::CloudRequirementsTomlBundle::default())
        }
        .await;
        if let Ok(application) = local {
            let _ = policy.local.publish(
                policy.local.policy().revision(),
                destination_policy(application.as_ref()),
            );
        }
        policy
    }

    /// Attaches the live application policy before an environment can start connecting.
    #[must_use]
    pub fn bind(
        &self,
        factory: codex_http_client::HttpClientFactory,
    ) -> codex_http_client::HttpClientFactory {
        factory.with_network_policy(self.effective.policy())
    }

    /// Publishes successfully loaded requirements before caller-owned clients are created.
    pub fn activate(&self, config: &mut Config) {
        let application = config
            .config_layer_stack
            .requirements()
            .application
            .as_ref()
            .map(|requirements| &requirements.value);
        let _ = self.effective.publish(
            self.effective.policy().revision(),
            destination_policy(application),
        );
        self.bind_config(config);
    }

    /// Keeps a rebuilt caller config on the live policy owned by the running app-server.
    pub fn bind_config(&self, config: &mut Config) {
        config.application_network_policy = self.effective.policy();
    }

    /// Limits cloud discovery and each authentication request to their exact local-policy endpoints.
    #[must_use]
    pub fn bind_bootstrap_auth(
        &self,
        mut auth: codex_login::AuthConfig,
    ) -> codex_login::AuthConfig {
        let factory = auth
            .auth_route_config
            .http_client_factory()
            .clone()
            .with_network_policy(self.local.policy());
        let endpoint = codex_backend_client::Client::new(
            auth.chatgpt_base_url
                .as_deref()
                .unwrap_or("https://chatgpt.com/backend-api/"),
            factory.clone(),
        )
        .config_bundle_url();
        auth.auth_route_config = codex_login::AuthRouteConfig::from_http_client_factory(
            factory.clone().with_network_policy(
                self.local
                    .policy()
                    .restrict_to_endpoints(endpoint.parse().into_iter().collect()),
            ),
        )
        .with_local_bootstrap_factory(factory)
        .with_application_network_policy(self.effective.policy());
        auth
    }
}

/// Destinations that application requirements allow.
#[must_use]
pub fn destination_policy(application: Option<&ApplicationRequirementsToml>) -> DestinationPolicy {
    match application.and_then(|application| application.network.as_ref()) {
        Some(network) if network.enabled => DestinationPolicy::Restricted {
            allowed_hosts: network
                .domains
                .iter()
                .filter(|&(_host, permission)| *permission == NetworkDomainPermissionToml::Allow)
                .map(|(host, _permission)| host.clone())
                .collect(),
        },
        Some(_) | None => DestinationPolicy::Unrestricted,
    }
}
