//! Notification-driven catalog replacement without reconnecting session state.

use std::sync::Arc;
use std::time::Duration;

use codex_rmcp_client::RmcpClient;

use super::McpConnectionSet;
use crate::mcp::CODEX_APPS_MCP_SERVER_NAME;
use crate::rmcp_client::ManagedClient;
use crate::rmcp_client::list_tools_for_client_uncached;
use crate::tools::ToolInfo;

pub(super) struct CatalogSnapshot {
    client: Arc<RmcpClient>,
    revision: u64,
    tools: Vec<ToolInfo>,
}

impl McpConnectionSet {
    pub(super) async fn refreshed_tools_for(
        &self,
        server: &str,
        client: &ManagedClient,
    ) -> Option<Vec<ToolInfo>> {
        self.refreshed_tools
            .read()
            .await
            .get(server)
            .filter(|snapshot| Arc::ptr_eq(&snapshot.client, &client.client))
            .map(|snapshot| snapshot.tools.clone())
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize refreshes and publish replacements under the captured-call revision lock"
    )]
    pub(crate) async fn refresh_changed_tools(&self) {
        let _refresh = self.tool_refresh_lock.lock().await;
        for (server, view) in &self.servers {
            // Apps have their own entitlement-aware publication path.
            if server == CODEX_APPS_MCP_SERVER_NAME {
                continue;
            }
            let Some(transport) = view.connection.client.ready_transport() else {
                continue;
            };
            let Ok(revision) = transport.tool_list_revision().await else {
                continue;
            };
            let observed = self
                .refreshed_tools
                .read()
                .await
                .get(server)
                .filter(|snapshot| Arc::ptr_eq(&snapshot.client, &transport))
                .map_or(0, |snapshot| snapshot.revision);
            if observed == revision {
                continue;
            }
            let Ok(client) = view.connection.client().await else {
                continue;
            };
            if !Arc::ptr_eq(&client.client, &transport) {
                continue;
            }
            let tools = list_tools_for_client_uncached(
                server,
                /*is_codex_apps_mcp_server*/ false,
                /*codex_apps_refresh_trigger*/ "notification",
                &transport,
                Some(
                    view.tool_timeout
                        .unwrap_or(Duration::from_secs(30))
                        .min(Duration::from_secs(30)),
                ),
                view.catalog_item_limit,
                client.server_instructions.as_deref(),
            )
            .await;
            let tools = match tools {
                Ok(tools) => tools,
                Err(error) => {
                    tracing::warn!(server, %error, "MCP catalog refresh failed; retaining last complete catalog");
                    continue;
                }
            };
            // A paginated listing spanning a change is not an atomic catalog.
            if transport.tool_list_revision().await.ok() != Some(revision) {
                continue;
            }
            let mut catalog_revision = self.tool_catalog_revision.write().await;
            self.refreshed_tools.write().await.insert(
                server.clone(),
                CatalogSnapshot {
                    client: transport,
                    revision,
                    tools,
                },
            );
            *catalog_revision += 1;
        }
    }
}
