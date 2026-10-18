//! Connection-owned, thread-scoped MCP overlay RPCs.

use super::*;
use crate::subscription_lifetimes::SubscriptionLifetimes;
use codex_app_server_protocol::ThreadExtensionAttachParams;
use codex_app_server_protocol::ThreadExtensionAttachResponse;
use codex_app_server_protocol::ThreadExtensionDetachParams;
use codex_app_server_protocol::ThreadExtensionDetachResponse;
use codex_config::McpServerConfig;

const MAX_EXTENSION_INSTRUCTIONS_BYTES: usize = 8 * 1024;

pub(super) struct ExtensionLease {
    owner: ConnectionId,
    lease_id: String,
    server_name: String,
    server_config: serde_json::Value,
    developer_instructions: String,
    pub(super) cancellation: Arc<CancellationToken>,
    ready: bool,
}

struct PendingConnectionAttach {
    leases: Arc<Mutex<HashMap<ThreadId, ExtensionLease>>>,
    thread: Arc<CodexThread>,
    thread_id: ThreadId,
    lease_id: String,
    cancellation: Arc<CancellationToken>,
    committed: bool,
}

impl Drop for PendingConnectionAttach {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.cancellation.cancel();
        let leases = Arc::clone(&self.leases);
        let thread = Arc::clone(&self.thread);
        let thread_id = self.thread_id;
        let lease_id = self.lease_id.clone();
        let cancellation = Arc::clone(&self.cancellation);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                {
                    let mut leases = leases.lock().await;
                    if leases
                        .get(&thread_id)
                        .is_some_and(|lease| lease.lease_id == lease_id)
                    {
                        leases.remove(&thread_id);
                    }
                }
                thread.detach_mcp_extension(&cancellation).await;
            });
        }
    }
}

impl ThreadRequestProcessor {
    pub(crate) async fn extension_attach(
        &self,
        owner: ConnectionId,
        params: ThreadExtensionAttachParams,
    ) -> Result<ThreadExtensionAttachResponse, JSONRPCErrorError> {
        let subscription_lifetimes: Arc<SubscriptionLifetimes> =
            Arc::clone(&self.thread_state_manager.subscription_lifetimes);
        let operation = async {
            let ThreadExtensionAttachParams {
                thread_id,
                server_name,
                server_config,
                developer_instructions,
            } = params;
            let thread_id = ThreadId::from_string(&thread_id)
                .map_err(|error| invalid_params(format!("invalid thread id: {error}")))?;
            if server_name.is_empty()
                || server_name.len() > 64
                || !server_name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            {
                return Err(invalid_params(
                    "serverName must be 1–64 ASCII letters, digits, '_' or '-'",
                ));
            }
            if developer_instructions.is_empty()
                || developer_instructions.len() > MAX_EXTENSION_INSTRUCTIONS_BYTES
            {
                return Err(invalid_params(
                    "developerInstructions must be nonempty and at most 8192 bytes",
                ));
            }
            let config: McpServerConfig = serde_json::from_value(server_config.clone())
                .map_err(|error| invalid_params(format!("invalid serverConfig: {error}")))?;
            if !config.is_local_environment()
                || !matches!(&config.transport, McpServerTransportConfig::Stdio { command, .. } if !command.is_empty())
            {
                return Err(invalid_params(
                    "serverConfig must be a local stdio MCP server",
                ));
            }
            let thread = self
                .thread_manager
                .get_thread(thread_id)
                .await
                .map_err(|_| invalid_request(format!("thread not loaded: {thread_id}")))?;
            let (lease_id, cancellation) = {
                let mut leases = self.extension_leases.lock().await;
                // An unsubscribe or disconnect that began after this attach has already scanned
                // the leases, so an owner without a subscription must not insert one.
                if !subscription_lifetimes.contains(owner, thread_id) {
                    return Err(invalid_request("extension lease owner disconnected"));
                }
                if leases
                    .get(&thread_id)
                    .is_some_and(|lease| lease.cancellation.is_cancelled())
                {
                    leases.remove(&thread_id);
                }
                if let Some(existing) = leases.get(&thread_id) {
                    if existing.owner == owner
                        && existing.server_name == server_name
                        && existing.server_config == server_config
                        && existing.developer_instructions == developer_instructions
                        && !existing.cancellation.is_cancelled()
                    {
                        if !existing.ready {
                            return Err(invalid_request("extension lease pending"));
                        }
                        return Ok(ThreadExtensionAttachResponse {
                            lease_id: existing.lease_id.clone(),
                        });
                    }
                    return Err(invalid_request("extension lease conflict"));
                }
                let lease_id = Uuid::now_v7().to_string();
                let cancellation = Arc::new(CancellationToken::new());
                leases.insert(
                    thread_id,
                    ExtensionLease {
                        owner,
                        lease_id: lease_id.clone(),
                        server_name: server_name.clone(),
                        server_config,
                        developer_instructions: developer_instructions.clone(),
                        cancellation: cancellation.clone(),
                        ready: false,
                    },
                );
                (lease_id, cancellation)
            };
            let mut pending = PendingConnectionAttach {
                leases: Arc::clone(&self.extension_leases),
                thread: Arc::clone(&thread),
                thread_id,
                lease_id: lease_id.clone(),
                cancellation: Arc::clone(&cancellation),
                committed: false,
            };
            if let Err(error) = thread
                .attach_mcp_extension(
                    server_name,
                    config,
                    developer_instructions,
                    cancellation.clone(),
                )
                .await
            {
                return Err(invalid_request(error.to_string()));
            }
            if cancellation.is_cancelled() {
                return Err(invalid_request("extension lease owner disconnected"));
            }
            let still_owned = {
                let mut leases = self.extension_leases.lock().await;
                if let Some(lease) = leases.get_mut(&thread_id)
                    && lease.lease_id == lease_id
                    && !lease.cancellation.is_cancelled()
                {
                    lease.ready = true;
                    true
                } else {
                    false
                }
            };
            if !still_owned {
                return Err(invalid_request("extension lease owner disconnected"));
            }
            pending.committed = true;
            Ok(ThreadExtensionAttachResponse { lease_id })
        };
        tokio::time::timeout(Duration::from_secs(120), operation)
            .await
            .map_err(|_| invalid_request("extension MCP server did not become ready"))?
    }

    pub(crate) async fn extension_detach(
        &self,
        owner: ConnectionId,
        params: ThreadExtensionDetachParams,
    ) -> Result<ThreadExtensionDetachResponse, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_params(format!("invalid thread id: {error}")))?;
        let cancellation = {
            let mut leases = self.extension_leases.lock().await;
            if leases
                .get(&thread_id)
                .is_some_and(|lease| lease.owner == owner && lease.lease_id == params.lease_id)
            {
                leases.remove(&thread_id).map(|lease| {
                    lease.cancellation.cancel();
                    lease.cancellation
                })
            } else {
                None
            }
        };
        if let Some(cancellation) = cancellation
            && let Ok(thread) = self.thread_manager.get_thread(thread_id).await
        {
            thread.detach_mcp_extension(&cancellation).await;
        }
        Ok(ThreadExtensionDetachResponse {})
    }

    pub(crate) async fn revoke_extension_connection(&self, owner: ConnectionId) {
        let revoked = {
            let mut leases = self.extension_leases.lock().await;
            leases
                .extract_if(|_, lease| lease.owner == owner)
                .map(|(thread_id, lease)| {
                    lease.cancellation.cancel();
                    (thread_id, lease.cancellation)
                })
                .collect::<Vec<_>>()
        };
        for (thread_id, cancellation) in revoked {
            if let Ok(thread) = self.thread_manager.get_thread(thread_id).await {
                thread.detach_mcp_extension(&cancellation).await;
            }
        }
    }

    /// Unsubscribe can overtake an attach response while MCP startup is pending.
    pub(crate) async fn revoke_extension_subscription(&self, owner: ConnectionId, thread_id: &str) {
        let Ok(thread_id) = ThreadId::from_string(thread_id) else {
            return;
        };
        // Withdraw admission before scanning, so an attach still in flight cannot insert a lease
        // after the scan.
        self.thread_state_manager
            .subscription_lifetimes
            .remove(owner, thread_id);
        let cancellation = {
            let mut leases = self.extension_leases.lock().await;
            if leases
                .get(&thread_id)
                .is_some_and(|lease| lease.owner == owner)
            {
                leases.remove(&thread_id).map(|lease| {
                    lease.cancellation.cancel();
                    lease.cancellation
                })
            } else {
                None
            }
        };
        if let Some(cancellation) = cancellation
            && let Ok(thread) = self.thread_manager.get_thread(thread_id).await
        {
            thread.detach_mcp_extension(&cancellation).await;
        }
    }
}
