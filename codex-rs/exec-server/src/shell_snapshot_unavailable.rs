//! Compatibility seam for credential snapshots without a managed proxy runtime.

use codex_network_proxy::NetworkProxy;
use codex_network_proxy::RemoteCredentialBrokerCache;

use super::CaptureResult;
use super::SnapshotReplay;
use crate::process_sandbox::PreparedExecRequest;
use crate::protocol::ExecParams;

pub(super) fn restore_fallback(prepared: &mut PreparedExecRequest) {
    if let Some(handle) = &prepared.network_proxy_handle {
        match *handle {}
    }
}

#[expect(
    clippy::unused_async,
    reason = "The uninhabited seam retains the upstream async call signature"
)]
pub(super) async fn prepare(
    _params: &ExecParams,
    _output: &[u8],
    _replay: SnapshotReplay,
    proxy: &NetworkProxy,
    _credential_brokers: &RemoteCredentialBrokerCache,
) -> CaptureResult {
    match *proxy {}
}
