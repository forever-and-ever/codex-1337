//! Connection-scoped MCP server authority for an already loaded thread.

use super::*;
use crate::mcp::McpRuntimeProjection;
use codex_config::McpServerConfig;
use codex_mcp::McpServerRegistration;

pub(crate) const CONNECTION_LEASE_SOURCE_ID: &str = "connection_lease";
pub(super) const EXTENSION_INSTRUCTION_MARKER: &str = "[connection-scoped MCP extension]";
const ATTACH_READINESS_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);
pub(super) const DETACHED_INSTRUCTIONS: &str = "The connection-scoped MCP extension is detached. Its previous developer instructions are inactive; do not call its MCP tools until a new lease is attached.";

#[derive(Clone)]
pub(super) struct McpExtensionLease {
    pub(super) server_name: String,
    pub(super) server_config: McpServerConfig,
    pub(super) developer_instructions: String,
    pub(super) cancellation: Arc<CancellationToken>,
    pub(super) instruction_pending: bool,
    pub(super) deactivation_emitted: bool,
}

struct PendingMcpExtensionAttach {
    session: Arc<Session>,
    cancellation: Arc<CancellationToken>,
    committed: bool,
}

impl Drop for PendingMcpExtensionAttach {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.cancellation.cancel();
        let removed = {
            let Ok(mut lease) = self.session.mcp_extension.write() else {
                self.session
                    .mark_mcp_runtime_dirty(McpRefreshReason::ExplicitInvalidation);
                self.session.schedule_mcp_prewarm();
                return;
            };
            if lease
                .as_ref()
                .is_some_and(|lease| Arc::ptr_eq(&lease.cancellation, &self.cancellation))
            {
                lease.take().is_some()
            } else {
                false
            }
        };
        if removed {
            if let Ok(mut delta) = self.session.mcp_extension_context_delta.lock() {
                *delta = Some(DETACHED_INSTRUCTIONS.to_string());
            }
            self.session
                .mark_mcp_runtime_dirty(McpRefreshReason::ExplicitInvalidation);
            self.session.schedule_mcp_prewarm();
        }
    }
}

impl Session {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "waits for an in-flight terminal publication before checking for an active turn"
    )]
    pub(crate) async fn attach_mcp_extension(
        self: &Arc<Self>,
        server_name: String,
        server_config: McpServerConfig,
        developer_instructions: String,
        cancellation: Arc<CancellationToken>,
    ) -> anyhow::Result<()> {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => anyhow::bail!("extension MCP lease revoked"),
            result = tokio::time::timeout(ATTACH_READINESS_TIMEOUT, async {
                let _admission = self.mcp_extension_admission.acquire().await.unwrap_or_else(|_| {
                    unreachable!("extension admission semaphore is never closed")
                });
                {
                    let _publication = self.terminal_publication.lock().await;
                    if self.active_turn.lock().await.is_some() {
                        anyhow::bail!("thread has an active turn");
                    }
                }
                Box::pin(self.refresh_mcp_if_dirty()).await;
                if self
                    .services
                    .mcp_runtime
                    .current_config()
                    .is_some_and(|config| config.mcp_server_catalog.servers().any(|(key, _)| key.name() == server_name))
                {
                    anyhow::bail!("extension lease conflict: MCP server name is already registered");
                }
                let instruction_pending = self.last_mcp_extension_instruction.lock()
                    .map_err(|_| anyhow::anyhow!("MCP extension instruction history poisoned"))?.as_ref()
                    != Some(&developer_instructions);
                {
                    let mut lease = self.mcp_extension.write()
                        .map_err(|_| anyhow::anyhow!("MCP extension lease authority poisoned"))?;
                    if lease.is_some() {
                        anyhow::bail!("extension lease conflict: thread already has an extension");
                    }
                    *lease = Some(McpExtensionLease {
                        server_name: server_name.clone(),
                        server_config,
                        developer_instructions,
                        cancellation: cancellation.clone(),
                        instruction_pending,
                        deactivation_emitted: false,
                    });
                }
                let mut pending = PendingMcpExtensionAttach {
                    session: Arc::clone(self),
                    cancellation: cancellation.clone(),
                    committed: false,
                };
                *self.mcp_extension_context_delta.lock().map_err(|_| anyhow::anyhow!("MCP extension instruction delta poisoned"))? =
                    (!instruction_pending).then(|| format!(
                        "The connection-scoped MCP extension `{server_name}` is active again. Its previously supplied developer instructions apply while this lease remains active."
                    ));
                self.services
                    .mcp_runtime
                    .set_server_lease(server_name.clone(), cancellation.clone());
                self.mark_mcp_runtime_dirty(McpRefreshReason::ExplicitInvalidation);
                Box::pin(self.refresh_mcp_if_dirty()).await;
                let ready = self.services
                    .mcp_runtime
                    .activate_server_lease(&server_name, &cancellation)
                    .await;
                if !ready || cancellation.is_cancelled() {
                    anyhow::bail!("extension MCP server did not become ready");
                }
                pending.committed = true;
                Ok(())
            }) => result.map_err(|_| anyhow::anyhow!("extension MCP server did not become ready"))?,
        }
    }

    #[expect(
        clippy::panic,
        reason = "Poisoned lease authority cannot safely revoke capabilities; this infallible teardown must fail closed"
    )]
    pub(crate) fn detach_mcp_extension(self: &Arc<Self>, cancellation: &Arc<CancellationToken>) {
        cancellation.cancel();
        let removed = {
            let mut lease = self
                .mcp_extension
                .write()
                .unwrap_or_else(|_| panic!("MCP extension lease authority poisoned"));
            if lease
                .as_ref()
                .is_some_and(|lease| Arc::ptr_eq(&lease.cancellation, cancellation))
            {
                lease.take()
            } else {
                None
            }
        };
        if let Some(lease) = removed {
            if !lease.deactivation_emitted {
                *self
                    .mcp_extension_context_delta
                    .lock()
                    .unwrap_or_else(|_| panic!("MCP extension instruction delta poisoned")) =
                    Some(DETACHED_INSTRUCTIONS.to_string());
            }
            self.mark_mcp_runtime_dirty(McpRefreshReason::ExplicitInvalidation);
            self.schedule_mcp_prewarm();
        }
    }

    #[expect(
        clippy::panic,
        reason = "Poisoned instruction authority cannot safely choose active instructions; infallible history projection must fail closed"
    )]
    pub(super) fn mcp_extension_instruction(&self, full_context: bool) -> Option<String> {
        let mut lease = self
            .mcp_extension
            .write()
            .unwrap_or_else(|_| panic!("MCP extension lease authority poisoned"));
        let mut delta = self
            .mcp_extension_context_delta
            .lock()
            .unwrap_or_else(|_| panic!("MCP extension instruction delta poisoned"));
        let mut last_instruction = self
            .last_mcp_extension_instruction
            .lock()
            .unwrap_or_else(|_| panic!("MCP extension instruction history poisoned"));
        let Some(lease) = lease.as_mut() else {
            return delta.take();
        };
        if lease.cancellation.is_cancelled() {
            if !lease.deactivation_emitted {
                lease.deactivation_emitted = true;
                return Some(DETACHED_INSTRUCTIONS.to_string());
            }
            return delta.take();
        }
        if full_context {
            delta.take();
        }
        if !full_context && !lease.instruction_pending {
            return delta.take();
        }
        lease.instruction_pending = false;
        let instructions = lease.developer_instructions.clone();
        *last_instruction = Some(instructions.clone());
        Some(format!(
            "{EXTENSION_INSTRUCTION_MARKER} These instructions replace all previously supplied connection-scoped MCP extension instructions. They apply only while this thread has a live lease for MCP server `{}`. Without that lease they are inactive.\n{instructions}",
            lease.server_name
        ))
    }

    #[expect(
        clippy::panic,
        reason = "Poisoned lease authority cannot safely expose capabilities; infallible MCP projection must fail closed"
    )]
    pub(super) fn project_mcp_extension(
        &self,
        projection: &mut McpRuntimeProjection,
        environments: &TurnEnvironmentSnapshot,
        desired_config: &Config,
    ) {
        let lease = self
            .mcp_extension
            .read()
            .unwrap_or_else(|_| panic!("MCP extension lease authority poisoned"));
        let Some(lease) = lease.as_ref() else {
            return;
        };
        if lease.cancellation.is_cancelled() {
            return;
        }
        if projection
            .config
            .mcp_server_catalog
            .servers()
            .any(|(key, _)| key.name() == lease.server_name)
        {
            lease.cancellation.cancel();
            tracing::warn!(
                server = lease.server_name,
                "extension MCP server name now conflicts with host configuration"
            );
            return;
        }
        let mut catalog = projection.config.mcp_server_catalog.to_builder();
        let mut server_config = lease.server_config.clone();
        if let Some(requirements) = desired_config
            .config_layer_stack
            .requirements()
            .mcp_servers
            .as_ref()
            && !requirements
                .value
                .get(&lease.server_name)
                .is_some_and(|requirement| server_config.matches_requirement(requirement))
        {
            server_config.enabled = false;
            server_config.disabled_reason =
                Some(codex_config::McpServerDisabledReason::Requirements {
                    source: requirements.source.clone(),
                });
        }
        catalog.register(McpServerRegistration::from_extension(
            lease.server_name.clone(),
            CONNECTION_LEASE_SOURCE_ID,
            /*contribution_order*/ 0,
            server_config,
        ));
        let selections = environments.all_selections();
        let scope = McpEnvironmentScope::Selected(&selections);
        projection.config.mcp_server_catalog =
            catalog.build_with_environment_authority(|id| scope.authority_for(id));
    }
}
