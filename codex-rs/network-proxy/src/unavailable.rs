//! Compatibility API for managed proxy requests. No proxy runtime can be constructed.

use crate::config;
use crate::network_policy::NetworkPolicyDecider;
use crate::runtime::NetworkProxyState;
use anyhow::Result;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

pub use crate::proxy_environment::*;

/// Selects how managed sandbox clients reach their logical proxy instance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ManagedProxyRouting {
    /// Use shared SID-attributed ingress when available and dedicated listeners otherwise.
    #[default]
    SharedIngress,
    /// Reserve private loopback TCP ports for sandboxes that enforce endpoint access directly.
    DedicatedListeners,
}

/// Source-compatible request builder. No arguments can make this capability available.
#[derive(Clone, Default)]
pub struct NetworkProxyBuilder;

impl NetworkProxyBuilder {
    pub fn state(self, state: Arc<NetworkProxyState>) -> Self {
        let _ = state;
        self
    }

    pub fn managed_by_codex(self, managed_by_codex: bool) -> Self {
        let _ = managed_by_codex;
        self
    }

    pub fn managed_proxy_routing(self, managed_proxy_routing: ManagedProxyRouting) -> Self {
        let _ = managed_proxy_routing;
        self
    }

    pub fn policy_decider_arc(self, decider: Arc<dyn NetworkPolicyDecider>) -> Self {
        let _ = decider;
        self
    }

    pub async fn build(self) -> Result<NetworkProxy> {
        Err(crate::unsupported_error().into())
    }
}

/// Portable managed-network facts needed by an operating-system sandbox.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedNetworkSandboxContext {
    /// Loopback proxy ports that sandboxed commands may connect to.
    #[serde(default)]
    pub loopback_ports: Vec<u16>,
    /// Whether the command may bind local sockets and exchange loopback traffic.
    #[serde(default)]
    pub allow_local_binding: bool,
    /// Unix-domain socket paths allowed by the effective managed-network policy.
    #[serde(default)]
    pub allow_unix_sockets: Vec<String>,
    /// Whether the effective policy permits connections to all Unix-domain sockets.
    #[serde(default)]
    pub dangerously_allow_all_unix_sockets: bool,
}

/// Environment-specific managed-network settings prepared for one command launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedManagedNetwork {
    /// Complete command environment with managed proxy variables applied.
    pub env: HashMap<String, String>,
    /// Matching portable sandbox inputs for the command environment.
    pub sandbox_context: ManagedNetworkSandboxContext,
}

/// An unavailable capability: this type has no values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkProxy {}

#[allow(
    unused_variables,
    reason = "Compatibility signatures for an uninhabited runtime"
)]
impl NetworkProxy {
    #[must_use]
    pub fn builder() -> NetworkProxyBuilder {
        NetworkProxyBuilder
    }
    #[must_use]
    pub fn http_addr(&self) -> SocketAddr {
        match *self {}
    }
    #[must_use]
    pub fn socks_addr(&self) -> SocketAddr {
        match *self {}
    }
    #[cfg(target_os = "windows")]
    pub fn network_proxy_restricting_sid(&self, environment_id: Option<&str>) -> Option<String> {
        match *self {}
    }
    pub async fn current_cfg(&self) -> Result<config::NetworkProxyConfig> {
        match *self {}
    }
    pub async fn remote_launch_config(
        &self,
        local_binding_policy: crate::LocalBindingPolicy,
    ) -> Result<crate::RemoteNetworkProxyLaunchConfig> {
        match *self {}
    }
    #[must_use]
    pub fn remote_policy_decider(
        &self,
        allow_local_binding: bool,
    ) -> Option<Arc<dyn NetworkPolicyDecider>> {
        match *self {}
    }
    #[must_use]
    pub fn allow_local_binding(&self) -> bool {
        match *self {}
    }
    #[must_use]
    pub fn allow_unix_sockets(&self) -> Arc<[String]> {
        match *self {}
    }
    #[must_use]
    pub fn dangerously_allow_all_unix_sockets(&self) -> bool {
        match *self {}
    }
    #[must_use]
    pub fn managed_mitm_ca_trust_bundle_path(&self) -> Option<AbsolutePathBuf> {
        match *self {}
    }
    pub fn apply_to_env(&self, env: &mut HashMap<String, String>) {
        match *self {}
    }
    pub fn restore_brokered_credentials(
        &self,
        env: &mut HashMap<String, String>,
        command: &mut [String],
    ) {
        match *self {}
    }
    pub fn restore_and_disable_brokered_credentials(
        &self,
        env: &mut HashMap<String, String>,
        command: &mut [String],
    ) {
        match *self {}
    }
    #[must_use]
    pub fn credential_broker_environment(
        &self,
        env: &HashMap<String, String>,
    ) -> crate::CredentialBrokerEnvironment {
        match *self {}
    }
    pub fn apply_to_env_for_optional_environment(
        &self,
        env: &mut HashMap<String, String>,
        environment_id: Option<&str>,
    ) -> Result<()> {
        match *self {}
    }
    pub fn prepare_for_optional_environment(
        &self,
        env: HashMap<String, String>,
        environment_id: Option<&str>,
    ) -> Result<PreparedManagedNetwork> {
        match *self {}
    }
    pub async fn run(&self) -> Result<NetworkProxyHandle> {
        match *self {}
    }
    pub fn for_execution(
        &self,
        environment_id: &str,
        execution_id: &str,
        attribution_token: String,
        environment_policy: Option<crate::EnvironmentNetworkPolicy>,
        fallback_policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
    ) -> Result<Self> {
        match *self {}
    }
}

/// An unavailable runtime has no running handle.
#[derive(Debug)]
pub enum NetworkProxyHandle {}

impl NetworkProxyHandle {
    pub async fn shutdown(self) -> Result<()> {
        match self {}
    }
}
