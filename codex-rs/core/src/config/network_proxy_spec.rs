use codex_config::NetworkConstraints;
use codex_execpolicy::Policy;
use codex_network_proxy::BlockedRequestObserver;
use codex_network_proxy::EnvironmentNetworkPolicy;
use codex_network_proxy::LocalBindingPolicy;
use codex_network_proxy::ManagedProxyRouting;
use codex_network_proxy::NetworkPolicyDecider;
use codex_network_proxy::NetworkProxy;
use codex_network_proxy::NetworkProxyAuditMetadata;
use codex_network_proxy::NetworkProxyConfig;
use codex_network_proxy::NetworkProxyConstraints;
use codex_network_proxy::NetworkProxyState;
use codex_network_proxy::host_and_port_from_network_addr;
#[cfg(any(target_os = "windows", test))]
use codex_network_proxy::managed_proxy_ports;
use codex_network_proxy::validate_policy_against_constraints;
use codex_protocol::models::PermissionProfile;
use codex_utils_path_uri::Platform;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkProxySpec {
    base_config: NetworkProxyConfig,
    requirements: Option<NetworkConstraints>,
    config: NetworkProxyConfig,
    constraints: NetworkProxyConstraints,
    hard_deny_allowlist_misses: bool,
}

/// The unavailable managed proxy runtime has no instances.
pub enum StartedNetworkProxy {}

impl StartedNetworkProxy {
    pub fn proxy(&self) -> NetworkProxy {
        match *self {}
    }
}

impl NetworkProxySpec {
    pub(crate) fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub(crate) fn credential_broker_enabled(&self) -> bool {
        self.enabled() && self.config.credential_broker && self.constraints.enabled != Some(false)
    }

    pub fn proxy_host_and_port(&self) -> String {
        host_and_port_from_network_addr(&self.config.proxy_url, /*default_port*/ 3128)
    }

    pub fn socks_enabled(&self) -> bool {
        self.config.enable_socks5
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn configured_proxy_ports(&self) -> std::io::Result<Vec<u16>> {
        managed_proxy_ports(&self.config).map_err(std::io::Error::other)
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn allow_local_binding(&self) -> bool {
        self.config.allow_local_binding()
    }

    /// Returns the firewall settings and listener identities used by the Windows provisioning service.
    #[cfg(target_os = "windows")]
    pub fn windows_sandbox_proxy_listeners(
        &self,
    ) -> std::io::Result<(
        codex_windows_sandbox::WindowsSandboxProvisioningSettings,
        codex_windows_sandbox::WindowsSandboxProxyListeners,
    )> {
        if !self.config.enabled {
            return Ok((
                codex_windows_sandbox::WindowsSandboxProvisioningSettings::default(),
                codex_windows_sandbox::WindowsSandboxProxyListeners::default(),
            ));
        }

        let proxy_ports = self.configured_proxy_ports()?;
        let http_port = self
            .proxy_host_and_port()
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .ok_or_else(|| std::io::Error::other("invalid HTTP proxy listener port"))?;
        let socks_port = self.config.enable_socks5.then(|| {
            proxy_ports
                .iter()
                .copied()
                .find(|port| *port != http_port)
                .unwrap_or(http_port)
        });
        Ok((
            codex_windows_sandbox::WindowsSandboxProvisioningSettings {
                proxy_ports,
                allow_local_binding: self.config.allow_local_binding(),
            },
            codex_windows_sandbox::WindowsSandboxProxyListeners {
                http_ports: vec![http_port],
                socks_ports: socks_port.into_iter().collect(),
            },
        ))
    }

    pub fn from_config_and_constraints(
        config: NetworkProxyConfig,
        requirements: Option<NetworkConstraints>,
        permission_profile: &PermissionProfile,
    ) -> std::io::Result<Self> {
        // A requirement that only disables the removed proxy remains enforceable.
        // Every other requested proxy-managed restriction must fail closed, even
        // if the controller feature was never enabled.
        if requirements.as_ref().is_some_and(|requirements| {
            requirements
                != &NetworkConstraints {
                    enabled: requirements.enabled.filter(|enabled| !enabled),
                    ..NetworkConstraints::default()
                }
        }) {
            return Err(codex_network_proxy::unsupported_error());
        }
        let base_config = config.clone();
        let _ = permission_profile;
        let mut config = config;
        let enabled = requirements
            .as_ref()
            .and_then(|requirements| requirements.enabled);
        if let Some(enabled) = enabled {
            config.enabled = enabled;
        }
        codex_network_proxy::reject_proxy_configuration(&config)?;
        let constraints = NetworkProxyConstraints {
            enabled,
            ..NetworkProxyConstraints::default()
        };
        validate_policy_against_constraints(&config, &constraints).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("network proxy constraints are invalid: {err}"),
            )
        })?;
        Ok(Self {
            base_config,
            requirements,
            config,
            constraints,
            hard_deny_allowlist_misses: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_proxy(
        &self,
        permission_profile: &PermissionProfile,
        managed_proxy_routing: ManagedProxyRouting,
        local_binding_policy: LocalBindingPolicy,
        policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
        blocked_request_observer: Option<Arc<dyn BlockedRequestObserver>>,
        enable_network_approval_flow: bool,
        audit_metadata: NetworkProxyAuditMetadata,
    ) -> std::io::Result<StartedNetworkProxy> {
        let _ = (
            permission_profile,
            managed_proxy_routing,
            local_binding_policy,
            policy_decider,
            blocked_request_observer,
            enable_network_approval_flow,
            audit_metadata,
        );
        Err(codex_network_proxy::unsupported_error())
    }

    pub(crate) fn recompute_for_permission_profile(
        &self,
        permission_profile: &PermissionProfile,
    ) -> std::io::Result<Self> {
        Self::from_config_and_constraints(
            self.base_config.clone(),
            self.requirements.clone(),
            permission_profile,
        )
    }

    /// Returns proxy activation and traffic policy without controller-owned runtime settings.
    pub fn environment_policy(&self) -> EnvironmentNetworkPolicy {
        EnvironmentNetworkPolicy::from_config(&self.config, self.hard_deny_allowlist_misses)
    }

    pub(crate) fn for_environment(
        controller: Option<&Self>,
        policy: &EnvironmentNetworkPolicy,
        permission_profile: &PermissionProfile,
        exec_policy: &Policy,
        local_binding_policy: LocalBindingPolicy,
    ) -> std::io::Result<Self> {
        let _ = (
            controller,
            policy,
            permission_profile,
            exec_policy,
            local_binding_policy,
        );
        Err(codex_network_proxy::unsupported_error())
    }

    pub(crate) fn build_state_with_audit_metadata(
        &self,
        audit_metadata: NetworkProxyAuditMetadata,
        executor_os: Platform,
        local_binding_policy: LocalBindingPolicy,
    ) -> std::io::Result<NetworkProxyState> {
        let _ = (audit_metadata, executor_os, local_binding_policy);
        Err(codex_network_proxy::unsupported_error())
    }
}
