//! Policy preparation only. Preparing a state cannot create a network proxy.
use crate::NetworkMode;
use crate::NetworkProxyConfig;
use crate::NetworkProxyConstraints;
use anyhow::Result;
use globset::GlobSet;
use serde::Deserialize;
use serde::Serialize;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkProxyAuditMetadata {
    pub conversation_id: Option<String>,
    pub app_version: Option<String>,
    pub user_account_id: Option<String>,
    pub auth_mode: Option<String>,
    pub originator: Option<String>,
    pub user_email: Option<String>,
    pub terminal_type: Option<String>,
    pub model: Option<String>,
    pub slug: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct BlockedRequest {
    pub host: String,
    pub reason: String,
    pub client: Option<String>,
    pub method: Option<String>,
    pub mode: Option<NetworkMode>,
    pub protocol: String,
    #[serde(skip)]
    pub execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub timestamp: i64,
}

pub struct BlockedRequestArgs {
    pub host: String,
    pub reason: String,
    pub client: Option<String>,
    pub method: Option<String>,
    pub mode: Option<NetworkMode>,
    pub protocol: String,
    pub decision: Option<String>,
    pub source: Option<String>,
    pub port: Option<u16>,
}

impl BlockedRequest {
    pub fn new(args: BlockedRequestArgs) -> Self {
        let BlockedRequestArgs {
            host,
            reason,
            client,
            method,
            mode,
            protocol,
            decision,
            source,
            port,
        } = args;
        Self {
            host,
            reason,
            client,
            method,
            mode,
            protocol,
            execution_id: None,
            decision,
            source,
            port,
            timestamp: unix_timestamp(),
        }
    }
}
fn unix_timestamp() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[derive(Clone)]
pub struct ConfigState {
    pub config: NetworkProxyConfig,
    pub allow_set: GlobSet,
    pub deny_set: GlobSet,
    pub constraints: NetworkProxyConstraints,
    pub blocked: VecDeque<BlockedRequest>,
    pub blocked_total: u64,
}
/// Compatibility observer for denial events from a managed network transport.
/// No transport is constructed in this build, so it emits no such events.
pub trait BlockedRequestObserver: Send + Sync + 'static {
    fn on_blocked_request(&self, request: BlockedRequest) -> BlockedRequestObserverFuture<'_>;
}

pub type BlockedRequestObserverFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

impl<O: BlockedRequestObserver + ?Sized> BlockedRequestObserver for Arc<O> {
    fn on_blocked_request(&self, request: BlockedRequest) -> BlockedRequestObserverFuture<'_> {
        Box::pin(async move { (**self).on_blocked_request(request).await })
    }
}

impl<F, Fut> BlockedRequestObserver for F
where
    F: Fn(BlockedRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn on_blocked_request(&self, request: BlockedRequest) -> BlockedRequestObserverFuture<'_> {
        Box::pin((self)(request))
    }
}

/// How the executor resolves and validates local binding when managed networking is enabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LocalBindingPolicy {
    /// Omission defaults to false; either explicit value is accepted.
    #[default]
    DefaultFalse,
    /// Omission defaults to true; explicit false is rejected.
    RequireTrue,
}

impl LocalBindingPolicy {
    /// Resolves omission without changing the configured value.
    pub fn resolve(self, config: &NetworkProxyConfig) -> bool {
        config
            .allow_local_binding
            .unwrap_or(config.enabled && self == Self::RequireTrue)
    }

    pub(crate) fn resolved_config(self, config: &NetworkProxyConfig) -> NetworkProxyConfig {
        NetworkProxyConfig {
            allow_local_binding: Some(self.resolve(config)),
            ..config.clone()
        }
    }
}

/// The removed proxy state has no instances.
#[derive(Clone, Debug)]
pub enum NetworkProxyState {}

#[allow(
    unused_variables,
    reason = "Compatibility signatures for an uninhabited runtime"
)]
impl NetworkProxyState {
    pub fn from_remote_launch_config(
        launch: crate::RemoteNetworkProxyLaunchConfig,
        executor_os: crate::Platform,
    ) -> Result<Self> {
        let _ = (launch, executor_os);
        Err(crate::unsupported_error().into())
    }
    pub fn set_proxy_private_ips_via_upstream(&mut self, enabled: bool) {
        match *self {}
    }
    pub fn set_policy_audit_observer(&mut self, observer: crate::NetworkPolicyAuditObserver) {
        match *self {}
    }
    pub fn set_launch_span_context(&mut self, span_context: opentelemetry::trace::SpanContext) {
        match *self {}
    }
    pub fn set_process_log_metadata(&mut self, metadata: crate::NetworkProxyProcessLogMetadata) {
        match *self {}
    }
}
