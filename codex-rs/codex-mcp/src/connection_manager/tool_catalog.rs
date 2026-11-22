#[path = "tool_catalog/boundary.rs"]
pub(crate) mod boundary;

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_config::McpStartupReadiness;
use codex_connectors::ConnectorRuntimeFetchSource;
use futures::future::join_all;
use rmcp::model::Tool;
use tracing::Instrument;
use tracing::instrument;
use tracing::trace;
use tracing::trace_span;

use super::McpConnectionSet;
use super::McpServerView;
use super::catalog_telemetry::emit_binding_catalog;
use super::catalog_telemetry::record_binding_catalog_size;
use super::catalog_telemetry::tool_definition_json_bytes;
use crate::AnnotationSource;
use crate::McpServerKey;
use crate::binding::McpBinding;
use crate::binding::PreparedMcpCall;
use crate::binding::PreparedToolInfo;
use crate::binding_clients::McpBindingClients;
use crate::client_tool_catalog::ClientToolCatalogRevision;
use crate::client_tool_catalog::CodexAppsToolSnapshot;
use crate::client_tool_catalog::NotificationRefresh;
use crate::client_tool_catalog::ToolCatalogRefresh;
use crate::client_tool_catalog::ToolCatalogSnapshot;
use crate::mcp::CODEX_APPS_MCP_SERVER_NAME;
use crate::rmcp_client::CODEX_APPS_REFRESH_DURATION_METRIC;
use crate::rmcp_client::MCP_TOOLS_LIST_DURATION_METRIC;
use crate::rmcp_client::ManagedClient;
use crate::rmcp_client::list_tools_for_client_uncached;
use crate::runtime::emit_duration;
use crate::tools::ToolInfo;
use crate::tools::normalize_tools_for_model_with_prefix;

const MCP_UI_META_KEY: &str = "ui";
const MCP_UI_VISIBILITY_META_KEY: &str = "visibility";
const MCP_UI_MODEL_VISIBILITY: &str = "model";
/// Returns whether a tool may be included in model-facing tool declarations.
///
/// Tools without visibility metadata remain visible. Tools with visibility
/// metadata are hidden unless they explicitly include `model`.
///
/// <https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx#resource-discovery>
pub fn tool_is_model_visible(tool: &ToolInfo) -> bool {
    let Some(visibility) = tool
        .tool
        .meta
        .as_deref()
        .and_then(|meta| meta.get(MCP_UI_META_KEY))
        .and_then(serde_json::Value::as_object)
        .and_then(|ui| ui.get(MCP_UI_VISIBILITY_META_KEY))
        .and_then(serde_json::Value::as_array)
    else {
        return true;
    };
    visibility
        .iter()
        .any(|target| target.as_str() == Some(MCP_UI_MODEL_VISIBILITY))
}

/// Catalog identity within one published connection set, including cached declarations.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum BindingCatalogRevision {
    Ready(ClientToolCatalogRevision),
    Cached(u64),
}

impl McpServerView {
    pub(super) async fn refresh_regular_tool_catalog(
        &self,
        server_name: &str,
        client: &ManagedClient,
        refresh: NotificationRefresh,
    ) -> Result<ToolCatalogRefresh> {
        if self.connection.client.is_codex_apps_mcp_server {
            return Ok(ToolCatalogRefresh::Current);
        }
        client
            .tool_catalog
            .refresh_if_changed(
                refresh,
                || client.client.tool_list_revision().current(),
                || async {
                    let relist = list_tools_for_client_uncached(
                        server_name,
                        /*is_codex_apps_mcp_server*/ false,
                        "list_changed",
                        &client.client,
                        self.tool_timeout,
                        self.catalog_item_limit,
                        client.server_instructions.as_deref(),
                    );
                    // Bound relist layouts for both revision checks and binding capture.
                    Box::pin(client.with_active_shared_lease(relist)).await?
                },
            )
            .await
            .inspect_err(|error| {
                tracing::warn!(server_name, error = %error, "MCP catalog refresh failed; retaining last successful catalog");
            })
    }
}

enum PendingCallToolInfo {
    BindingCatalog(usize),
    Owned(PreparedToolInfo),
}

impl McpConnectionSet {
    /// Observes opted-in servers at startup gates without waiting or cloning catalogs.
    pub(crate) fn record_startup_readiness(
        &self,
        phase: &'static str,
        required_servers: &[McpServerKey],
        required_plugins: &HashSet<String>,
    ) {
        if !tracing::enabled!(tracing::Level::INFO) {
            return;
        }
        for (server_name, view) in &self.servers {
            if view.startup_readiness != McpStartupReadiness::Catalog {
                continue;
            }
            let client = &view.connection.client;
            let startup_pending = !client.startup_complete.load(Ordering::Acquire);
            let cache_eligible = startup_pending
                && client
                    .tool_catalog_cache_context
                    .as_ref()
                    .is_some_and(|cache| {
                        cache
                            .current_revision_if(|tools| view.accepts_cached_tools(tools))
                            .is_some()
                    });
            let explicitly_required = required_servers.contains(server_name)
                || (self.is_selected_plugin_server(server_name)
                    && self
                        .plugin_id_for_server(server_name)
                        .is_some_and(|plugin_id| required_plugins.contains(plugin_id)));
            tracing::info!(
                event.name = "mcp.startup_readiness",
                server_name = %server_name,
                phase,
                startup_pending,
                startup_readiness = "catalog",
                cache_eligible,
                required = self.required_servers.binary_search(server_name).is_ok(),
                explicitly_required,
                "MCP startup readiness observed"
            );
        }
    }

    /// Returns all tools with model-visible names normalized.
    pub async fn list_all_tools(&self) -> Vec<ToolInfo> {
        Box::pin(self.list_tools_with_errors(|_| true)).await.0
    }

    #[instrument(level = "trace", skip_all, fields(mcp_server_count = self.servers.len()))]
    pub(crate) async fn list_tools_with_errors(
        &self,
        include_server: impl Fn(&McpServerKey) -> bool,
    ) -> (Vec<ToolInfo>, HashMap<McpServerKey, String>) {
        let (tools, errors, _) = self.list_catalogs_with_errors(include_server).await;
        (tools, errors)
    }

    pub(crate) async fn list_catalogs_with_errors(
        &self,
        include_server: impl Fn(&McpServerKey) -> bool,
    ) -> (
        Vec<ToolInfo>,
        HashMap<McpServerKey, String>,
        HashMap<McpServerKey, Arc<serde_json::Value>>,
    ) {
        let mut metadata = HashMap::new();
        let mut tools = Vec::new();
        let mut errors = HashMap::new();
        let mut available_server_count = 0;
        let mut unavailable_server_count = 0;
        let exposure = self
            .config
            .as_ref()
            .map(|config| config.mcp_server_catalog.exposure())
            .unwrap_or_default();
        let server_results = join_all(
            self.servers
                .iter()
                .filter(|(name, _)| include_server(name))
                .map(|(server_name, view)| async move {
                    view.connection.client.reconnect_failed_startup().await;
                    let has_cached_tools = view.connection.client.has_cached_tools();
                    let startup_complete = view
                        .connection
                        .client
                        .startup_complete
                        .load(Ordering::Acquire);
                    let server_tools = view
                        .listed_catalog(server_name.name(), &self.tool_plugin_context, exposure)
                        .instrument(trace_span!(
                            "list_tools_for_server",
                            server_name = %server_name,
                            has_cached_tools,
                            startup_complete
                        ))
                        .await;
                    let result = match server_tools {
                        Ok(catalog) => Ok(catalog),
                        Err(error) => {
                            trace!(
                                server_name = %server_name,
                                has_cached_tools,
                                startup_complete,
                                "MCP server tools unavailable while building tool list"
                            );
                            Err(error)
                        }
                    };
                    (server_name.to_owned(), result)
                }),
        )
        .await;
        for (server_name, server_tools) in server_results {
            match server_tools {
                Ok((server_tools, meta)) => {
                    if let Some(meta) = meta {
                        metadata.insert(server_name.clone(), meta);
                    }
                    available_server_count += 1;
                    tools.extend(server_tools);
                }
                Err(error) => {
                    unavailable_server_count += 1;
                    errors.insert(server_name.clone(), error.to_string());
                }
            }
        }
        let tools = normalize_tools_for_model_with_prefix(
            tools,
            self.prefix_mcp_tool_names,
            &self.non_prefixed_mcp_tool_servers,
            &self.reserved_native_tool_namespaces,
            exposure,
        );
        trace!(
            available_server_count,
            unavailable_server_count,
            tool_count = tools.len(),
            "built MCP tool list"
        );
        (tools, errors, metadata)
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) async fn prepare_call_for_tool(
        self: &Arc<Self>,
        config: Arc<crate::McpConfig>,
        advertised_tool: &ToolInfo,
    ) -> Result<PreparedMcpCall> {
        let server_name = advertised_tool.server_name();
        let unavailable = || advertised_tool.unavailable_to_model_error();
        let view = self
            .servers
            .get(advertised_tool.server_key())
            .ok_or_else(unavailable)?;
        if !view.tool_filter.allows(&advertised_tool.tool.name) {
            return Err(unavailable());
        }
        // Recovery belongs to readiness, before the caller acquires the tool execution lock.
        let mut client = view
            .connection
            .client()
            .await
            .with_context(|| format!("Could not start MCP server `{server_name}`"))?;
        client.tool_timeout = view.tool_timeout;
        if view
            .refresh_regular_tool_catalog(
                server_name,
                &client,
                NotificationRefresh::CallPreparation,
            )
            .await?
            != ToolCatalogRefresh::Current
        {
            return Err(unavailable());
        }
        let snapshot = client.tool_catalog.read(Arc::new).await;
        let current_tool = snapshot
            .tools
            .iter()
            .find(|tool| {
                tool.server_name() == server_name
                    && tool.tool.name == advertised_tool.tool.name
                    && tool.connector_id == advertised_tool.connector_id
            })
            .ok_or_else(unavailable)?;
        let mut tool_info = view
            .prepare_tools_for_model(
                vec![current_tool.clone()],
                &self.tool_plugin_context,
                config.mcp_server_catalog.exposure(),
            )
            .pop()
            .filter(tool_is_model_visible)
            .ok_or_else(unavailable)?;
        // A declaration advertised from a cache or a dormant server lacks annotations
        // the live server now supplies; those are authoritative. Any other change rejects.
        let same_declaration = |live: &rmcp::model::Tool, issued: &rmcp::model::Tool| {
            live == issued
                || (server_name == CODEX_APPS_MCP_SERVER_NAME
                    && crate::binding::same_apps_declaration(live, issued))
        };
        if !same_declaration(&tool_info.tool, &advertised_tool.tool) {
            if advertised_tool.annotation_source != AnnotationSource::Withheld {
                return Err(unavailable());
            }
            let mut withheld = tool_info.tool.clone();
            if advertised_tool.tool.annotations.is_none() {
                withheld.annotations = None;
            } else {
                withhold_read_only_hint(&mut withheld);
            }
            if !same_declaration(&withheld, &advertised_tool.tool) {
                return Err(unavailable());
            }
        }
        if tool_info.openai_file_input_optional_fields
            != advertised_tool.openai_file_input_optional_fields
        {
            return Err(unavailable());
        }
        // Preserve the globally normalized identity advertised to the model, while
        // taking schema, annotations, and approval metadata from the current catalog.
        tool_info
            .callable_namespace
            .clone_from(&advertised_tool.callable_namespace);
        tool_info
            .callable_name
            .clone_from(&advertised_tool.callable_name);
        self.prepare_call(tool_info, Arc::new(client), config, snapshot)
            .ok_or_else(unavailable)
    }

    #[instrument(level = "trace", skip_all)]
    pub(crate) async fn prepare_app_hook_call(
        self: &Arc<Self>,
        config: Arc<crate::McpConfig>,
        tool: &str,
    ) -> Result<PreparedMcpCall> {
        let server = McpServerKey::unscoped(CODEX_APPS_MCP_SERVER_NAME);
        self.validate_tool_call(&server, tool)?;
        let view = self
            .servers
            .get(&server)
            .context("Apps runtime is unavailable")?;
        let mut client = view
            .connection
            .client()
            .await
            .context("Could not start Apps server")?;
        client.tool_timeout = view.tool_timeout;
        let snapshot = client.tool_catalog.read(Arc::new).await;
        let mut matches = snapshot.tools.iter().filter(|info| info.tool.name == tool);
        let info = matches.next().context("App hook tool is unavailable")?;
        anyhow::ensure!(matches.next().is_none(), "App hook tool name is ambiguous");
        let exposure = config.mcp_server_catalog.exposure();
        let tools =
            view.prepare_tools_for_model(vec![info.clone()], &self.tool_plugin_context, exposure);
        let info = normalize_tools_for_model_with_prefix(
            tools,
            self.prefix_mcp_tool_names,
            &self.non_prefixed_mcp_tool_servers,
            &self.reserved_native_tool_namespaces,
            exposure,
        )
        .pop()
        .context("App hook tool is unavailable")?;
        self.prepare_call(info, Arc::new(client), config, snapshot)
            .context("App hook tool is unavailable")
    }

    fn prepare_call(
        self: &Arc<Self>,
        tool_info: impl Into<PreparedToolInfo>,
        client: Arc<ManagedClient>,
        config: Arc<crate::McpConfig>,
        tool_catalog_snapshot: Arc<ToolCatalogSnapshot>,
    ) -> Option<PreparedMcpCall> {
        let tool_info = tool_info.into();
        let (server_metadata, plugin_id, selected_plugin_server) = {
            let server_key = tool_info.get().server_key();
            let view = self.servers.get(server_key)?;
            (
                view.metadata.clone(),
                self.plugin_id_for_server(server_key).map(str::to_string),
                self.is_selected_plugin_server(server_key),
            )
        };
        PreparedMcpCall::new(
            Arc::clone(self),
            client,
            config,
            tool_catalog_snapshot,
            tool_info,
            server_metadata,
            plugin_id,
            selected_plugin_server,
        )
    }

    /// Refreshes one exact Apps catalog, preserving the raw inventory for app policy.
    pub(crate) async fn refresh_codex_apps_client_catalog<G>(
        &self,
        config: &crate::McpConfig,
        validate: impl FnOnce() -> Option<G>,
    ) -> Result<Option<CodexAppsToolSnapshot>> {
        let refresh_start = Instant::now();
        let _view = self
            .servers
            .get(&McpServerKey::unscoped(CODEX_APPS_MCP_SERVER_NAME))
            .ok_or_else(|| anyhow!("unknown MCP server '{CODEX_APPS_MCP_SERVER_NAME}'"))?;
        let Some((tools, _)) = self.refresh_codex_apps_tool_catalog(validate).await? else {
            return Ok(None);
        };

        emit_duration(
            CODEX_APPS_REFRESH_DURATION_METRIC,
            refresh_start.elapsed(),
            &[("path", "legacy"), ("trigger", "explicit")],
        );
        Ok(self.apps_tool_snapshot(config, tools))
    }

    /// Refreshes Apps tools and returns the prepared shared-cache winner for discovery.
    pub fn refresh_codex_apps_tools_for_discovery(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<ToolInfo>>> + Send + '_>>
    {
        Box::pin(async move {
            let refresh_start = Instant::now();
            let view = self
                .servers
                .get(&McpServerKey::unscoped(CODEX_APPS_MCP_SERVER_NAME))
                .ok_or_else(|| anyhow!("unknown MCP server '{CODEX_APPS_MCP_SERVER_NAME}'"))?;
            let (_, tools) = self
                .refresh_codex_apps_tool_catalog(|| Some(()))
                .await?
                .context("Apps discovery refresh skipped publication")?;
            let exposure = self
                .config
                .as_ref()
                .map(|config| config.mcp_server_catalog.exposure())
                .unwrap_or_default();
            let tools = view.prepare_tools_for_model(tools, &self.tool_plugin_context, exposure);
            let tools = normalize_tools_for_model_with_prefix(
                tools,
                self.prefix_mcp_tool_names,
                &self.non_prefixed_mcp_tool_servers,
                &self.reserved_native_tool_namespaces,
                exposure,
            );
            emit_duration(
                CODEX_APPS_REFRESH_DURATION_METRIC,
                refresh_start.elapsed(),
                &[("path", "legacy"), ("trigger", "explicit")],
            );
            Ok(tools)
        })
    }

    fn apps_tool_snapshot(
        &self,
        config: &crate::McpConfig,
        tools: Vec<ToolInfo>,
    ) -> Option<CodexAppsToolSnapshot> {
        let view = self
            .servers
            .get(&McpServerKey::unscoped(CODEX_APPS_MCP_SERVER_NAME))?;
        let server_has_permission = config
            .permission_profile_for_server(&McpServerKey::unscoped(CODEX_APPS_MCP_SERVER_NAME))
            .is_some();
        let model_visible_tool_names = tools
            .iter()
            .filter(|tool| {
                server_has_permission
                    && view.tool_filter.allows(&tool.tool.name)
                    && self
                        .tool_plugin_context
                        .allows_connector_id(tool.connector_id.as_deref())
                    && tool_is_model_visible(tool)
            })
            .map(|tool| tool.tool.name.to_string())
            .collect();
        Some(CodexAppsToolSnapshot {
            tools,
            model_visible_tool_names,
        })
    }

    pub(crate) async fn apps_tool_snapshot_from_mcp_client(
        &self,
        config: &crate::McpConfig,
    ) -> Option<CodexAppsToolSnapshot> {
        let view = self
            .servers
            .get(&McpServerKey::unscoped(CODEX_APPS_MCP_SERVER_NAME))?;
        let client = view.connection.client.ready_client()?;
        // The startup response was fetched uncached before shared-cache publication.
        let tools = client.startup_tools.as_ref()?.to_vec();
        self.apps_tool_snapshot(config, tools)
    }

    pub(crate) async fn apps_tools_from_mcp_client(&self) -> Vec<ToolInfo> {
        let Some(client) = self
            .servers
            .get(&McpServerKey::unscoped(CODEX_APPS_MCP_SERVER_NAME))
            .filter(|view| {
                view.connection
                    .client
                    .codex_apps_tools_cache_context
                    .is_none()
            })
            .and_then(|view| view.connection.client.ready_client())
        else {
            return Vec::new();
        };
        client
            .tool_catalog
            .read(|catalog| catalog.tools.to_vec())
            .await
    }

    /// Publishes the exact client catalog and returns both raw inventories.
    async fn refresh_codex_apps_tool_catalog<G>(
        &self,
        validate: impl FnOnce() -> Option<G>,
    ) -> Result<Option<(Vec<ToolInfo>, Vec<ToolInfo>)>> {
        let view = self
            .servers
            .get(&McpServerKey::unscoped(CODEX_APPS_MCP_SERVER_NAME))
            .ok_or_else(|| anyhow!("unknown MCP server '{CODEX_APPS_MCP_SERVER_NAME}'"))?;
        let managed_client = view
            .connection
            .client()
            .await
            .context("failed to get client")?;
        let Some((tools, list_start)) = managed_client
            .tool_catalog
            .refresh(
                || async {
                    let list_start = Instant::now();
                    let fetch_ticket = managed_client.codex_apps_tools_cache_context.as_ref().map(
                        |cache_context| {
                            cache_context.begin_fetch(ConnectorRuntimeFetchSource::HardRefresh)
                        },
                    );
                    let (client_tools, meta) = list_tools_for_client_uncached(
                        CODEX_APPS_MCP_SERVER_NAME,
                        /*is_codex_apps_mcp_server*/ true,
                        /*codex_apps_refresh_trigger*/ "explicit",
                        &managed_client.client,
                        view.tool_timeout,
                        view.catalog_item_limit,
                        managed_client.server_instructions.as_deref(),
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to refresh tools for MCP server '{CODEX_APPS_MCP_SERVER_NAME}'"
                        )
                    })?;
                    Ok((client_tools, meta, (fetch_ticket, list_start)))
                },
                validate,
                |client_tools, meta, (fetch_ticket, list_start)| {
                    // Discovery can accept another scope's winner; executable catalogs
                    // receive only the latest successful fetch from their own scope.
                    let tools = match (
                        managed_client.codex_apps_tools_cache_context.as_ref(),
                        fetch_ticket,
                    ) {
                        (Some(cache_context), Some(fetch_ticket)) => cache_context
                            .publish_runtime_if_newest_accepted(
                                fetch_ticket,
                                &managed_client.server_info,
                                client_tools.to_vec(),
                                meta,
                            )
                            .tools()
                            .to_vec(),
                        (None, None) => client_tools.to_vec(),
                        _ => unreachable!("Codex Apps fetch ticket requires cache context"),
                    };
                    (tools, list_start)
                },
            )
            .await?
        else {
            return Ok(None);
        };
        let client_tools = managed_client
            .tool_catalog
            .read(|catalog| catalog.tools.to_vec())
            .await;
        emit_duration(
            MCP_TOOLS_LIST_DURATION_METRIC,
            list_start.elapsed(),
            &[("cache", "miss")],
        );
        Ok(Some((client_tools, tools)))
    }
}

fn withhold_read_only_hint(tool: &mut Tool) {
    if let Some(annotations) = tool.annotations.as_mut() {
        annotations.read_only_hint = None;
    }
}
