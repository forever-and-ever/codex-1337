// Notification publication belongs to model boundaries, never attribution lookups.
use super::ToolInfo;
use crate::McpServerKey;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::Instrument;

use super::AnnotationSource;
use super::BindingCatalogRevision;
use super::ClientToolCatalogRevision;
use super::McpBinding;
use super::McpBindingClients;
use super::McpConnectionSet;
use super::McpStartupReadiness;
use super::NotificationRefresh;
use super::Ordering;
use super::PendingCallToolInfo;
use super::PreparedToolInfo;
use super::emit_binding_catalog;
use super::instrument;
use super::join_all;
use super::normalize_tools_for_model_with_prefix;
use super::record_binding_catalog_size;
use super::tool_definition_json_bytes;
use super::trace;
use super::withhold_read_only_hint;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogReadMode {
    Boundary,
    Current,
}

#[instrument(name = "mcp.binding.catalog_revision_wait", skip_all)]
pub(crate) async fn catalog_revisions(
    connections: &McpConnectionSet,
    required_servers: &[McpServerKey],
    required_plugins: &HashSet<String>,
    mode: CatalogReadMode,
) -> Option<HashMap<McpServerKey, BindingCatalogRevision>> {
    let mut revisions = HashMap::new();
    for (server_name, view) in &connections.servers {
        let startup_complete = view
            .connection
            .client
            .startup_complete
            .load(Ordering::Acquire);
        if !startup_complete
            || (view.startup_readiness == McpStartupReadiness::Catalog
                && view.connection.client.ready_client().is_none())
        {
            // Explicit requirements still wait for a live connection during capture.
            if !view.allows_cached_startup()
                || view.connection.client.cancel_token.is_cancelled()
                || required_servers
                    .iter()
                    .any(|required| required == server_name)
                || (connections.is_selected_plugin_server(server_name)
                    && connections
                        .plugin_id_for_server(server_name)
                        .is_some_and(|plugin_id| required_plugins.contains(plugin_id)))
            {
                return None;
            }
            let revision = view
                .connection
                .client
                .tool_catalog_cache_context
                .as_ref()?
                .current_revision_if(|tools| view.accepts_cached_tools(tools))?;
            revisions.insert(
                server_name.clone(),
                BindingCatalogRevision::Cached(revision),
            );
            continue;
        }
        let Some(client) = view.connection.client.ready_client() else {
            if !view.connection.client.is_codex_apps_mcp_server
                && connections
                    .required_servers
                    .binary_search(server_name)
                    .is_err()
                && matches!(view.connection.client.client.peek(), Some(Err(_)))
            {
                continue;
            }
            return None;
        };
        if client.client.is_closed().await {
            return None;
        }
        if mode == CatalogReadMode::Boundary {
            let _ = view
                .refresh_regular_tool_catalog(
                    server_name.name(),
                    &client,
                    NotificationRefresh::Boundary,
                )
                .await;
        } else if !view.connection.client.is_codex_apps_mcp_server
            && !client
                .tool_catalog
                .notifications_current(&client.client.tool_list_revision())
                .await
        {
            return None;
        }
        let revision = client.tool_catalog.read(|catalog| catalog.revision).await;
        revisions.insert(
            server_name.clone(),
            BindingCatalogRevision::Ready(ClientToolCatalogRevision {
                catalog: Arc::clone(&client.tool_catalog),
                revision,
            }),
        );
    }
    Some(revisions)
}

#[instrument(level = "trace", skip_all)]
pub(crate) async fn capture_binding_with_mode(
    connections: &Arc<McpConnectionSet>,
    config: Arc<crate::McpConfig>,
    plugins_available: bool,
    required_servers: &[McpServerKey],
    required_plugins: &HashSet<String>,
    mode: CatalogReadMode,
) -> McpBinding {
    let mut listed_tools = Vec::new();
    let mut clients = HashMap::new();
    let catalog_log_enabled = tracing::enabled!(
        target: "codex_otel.trace_safe",
        tracing::Level::INFO
    );
    let catalog_metrics = codex_otel::global();
    let catalog_measurement_enabled = catalog_log_enabled || catalog_metrics.is_some();
    let product_sku = codex_otel::bounded_product_sku(Some(
        config
            .apps_mcp_product_sku
            .as_deref()
            .unwrap_or(crate::mcp::DEFAULT_CODEX_APPS_MCP_PRODUCT_SKU),
    ))
    .unwrap_or("unknown");
    let optional_mcp_startup_grace = config.optional_mcp_startup_grace;
    let exposure = config.mcp_server_catalog.exposure();
    let server_snapshots = join_all(connections.servers.iter().map(|(server_name, view)| async move {
        if !view
            .connection
            .client
            .startup_complete
            .load(Ordering::Acquire)
        {
            if mode == CatalogReadMode::Current {
                return (server_name, view, view.cached_startup_tools(/*fallback*/ None));
            }
            let required = connections.required_servers.binary_search(server_name).is_ok();
            // Keep the catalog that lets us skip startup even if it expires during the wait.
            let cached_tools = view.cached_startup_tools(/*fallback*/ None);
            let has_cached_tools = cached_tools.is_some();
            let must_wait_for_startup = (required
                && (!view.allows_cached_startup() || !has_cached_tools))
                || required_servers
                    .iter()
                    .any(|required| required == server_name)
                || (connections.is_selected_plugin_server(server_name)
                    && connections
                        .plugin_id_for_server(server_name)
                        .is_some_and(|plugin_id| required_plugins.contains(plugin_id)))
                || (view.connection.client.is_codex_apps_mcp_server && !has_cached_tools);
            if !must_wait_for_startup && has_cached_tools {
                trace!(server_name = %server_name, "using cached MCP catalog without waiting for startup");
                return (server_name, view, cached_tools);
            }
            if !must_wait_for_startup && optional_mcp_startup_grace.is_zero() {
                if let Some(cache) = view.connection.client.tool_catalog_cache_context.as_ref()
                {
                    cache.optional_startup_deadline(
                        tokio::time::Instant::now(),
                        optional_mcp_startup_grace,
                    );
                }
                let _ = view.connection.client().await;
            } else if !must_wait_for_startup {
                let optional_startup_deadline = if view.connection.startup_is_dormant() {
                    tokio::time::Instant::now() + optional_mcp_startup_grace
                } else {
                    *connections.optional_startup_deadline.get_or_init(|| {
                        tokio::time::Instant::now() + optional_mcp_startup_grace
                    })
                };
                let startup_deadline = view
                    .connection
                    .client
                    .tool_catalog_cache_context
                    .as_ref()
                    .map_or(optional_startup_deadline, |cache| {
                        cache.optional_startup_deadline(
                            optional_startup_deadline,
                            optional_mcp_startup_grace,
                        )
                    });
                if tokio::time::timeout_at(startup_deadline, view.connection.client())
                    .await
                    .is_err()
                {
                    trace!(server_name = %server_name, "omitting pending optional MCP server");
                }
                return (server_name, view, cached_tools);
            }
            let _ = view.connection.client().await;
            return (server_name, view, cached_tools);
        }
        (server_name, view, None)
    }))
    .instrument(tracing::info_span!("mcp.binding.startup_wait"))
    .await;
    let server_results = join_all(server_snapshots.into_iter().map(|(server_name, view, cached_tools)| async move {
        let startup_pending = !view
            .connection
            .client
            .startup_complete
            .load(Ordering::Acquire);
        // A transient Apps startup failure must not remove functions already discovered
        // by the model. Keep retrying even when the cached catalog supplies this step.
        if mode == CatalogReadMode::Boundary {
            view.connection.client.reconnect_failed_startup().await;
        }
        let accepts_cached_catalog = view.startup_readiness == McpStartupReadiness::Catalog
            || (view.connection.client.is_codex_apps_mcp_server
                && !view.connection.client.cancel_token.is_cancelled());
        let cached_tools = if startup_pending
            || (accepts_cached_catalog && view.connection.client.ready_client().is_none())
        {
            view.cached_startup_tools(cached_tools)
        } else {
            None
        };
        let (client, server_tools, catalog_source) = if let Some(cached_tools) = cached_tools {
            (None, cached_tools, "cached")
        } else {
            // A server may opt out of caching after the first pass. Required catalog
            // readiness must then wait for discovery rather than silently omit its tools.
            if startup_pending
                && !(accepts_cached_catalog
                    && connections.required_servers.binary_search(server_name).is_ok())
            {
                return None;
            }
            let client = match mode {
                CatalogReadMode::Boundary => view.connection.client().await.ok(),
                CatalogReadMode::Current => view.connection.client.ready_client(),
            };
            let Some(mut client) = client else {
                trace!(server_name = %server_name, "omitting MCP server without an exact ready client");
                return None;
            };
            client.tool_timeout = view.tool_timeout;
            if mode == CatalogReadMode::Boundary {
                let _ = view.refresh_regular_tool_catalog(server_name.name(), &client, NotificationRefresh::Boundary).await;
            }
            let snapshot = client.tool_catalog.read(Arc::new).await;
            let server_tools = snapshot.tools.to_vec();
            (
                Some((Arc::new(client), snapshot)),
                server_tools,
                "live",
            )
        };
        let raw_definition_json_bytes = if catalog_measurement_enabled {
            tool_definition_json_bytes(server_tools.iter().map(|tool| &tool.tool))
        } else {
            0
        };
        if catalog_log_enabled {
            let plugin_id = connections.plugin_id_for_server(server_name);
            emit_binding_catalog(
                product_sku,
                if view.connection.client.is_codex_apps_mcp_server {
                    "codex_apps"
                } else if plugin_id.is_some() {
                    "plugin"
                } else {
                    "configured"
                },
                plugin_id,
                catalog_source,
                server_tools.len(),
                raw_definition_json_bytes,
            );
        }
        let server_tools = view.prepare_tools_for_model(server_tools, &connections.tool_plugin_context, exposure);
        let server_tools = server_tools
            .into_iter()
            .map(|mut tool| {
                if client.is_none() {
                    withhold_read_only_hint(&mut tool.tool);
                    tool.annotation_source = AnnotationSource::Withheld;
                }
                tool
            })
            .collect::<Vec<_>>();
        Some((
            server_name.clone(),
            client,
            server_tools,
            raw_definition_json_bytes,
        ))
    }))
    .instrument(tracing::info_span!("mcp.binding.catalog_capture"))
    .await;
    let mut raw_definition_json_bytes = 0usize;
    for (server_name, client, server_tools, server_definition_json_bytes) in
        server_results.into_iter().flatten()
    {
        raw_definition_json_bytes =
            raw_definition_json_bytes.saturating_add(server_definition_json_bytes);
        if let Some((client, snapshot)) = client {
            clients.insert(server_name, (client, snapshot));
        }
        listed_tools.extend(server_tools);
    }
    if let Some(metrics) = catalog_metrics.as_ref() {
        record_binding_catalog_size(metrics, product_sku, raw_definition_json_bytes);
    }
    let listed_tools = normalize_tools_for_model_with_prefix(
        listed_tools,
        connections.prefix_mcp_tool_names,
        &connections.non_prefixed_mcp_tool_servers,
        &connections.reserved_native_tool_namespaces,
        exposure,
    );
    let mut tools = Vec::with_capacity(listed_tools.len());
    let mut pending_calls = Vec::with_capacity(listed_tools.len());
    for tool_info in listed_tools {
        let model_visible = crate::tool_is_model_visible(&tool_info);
        let Some((client, snapshot)) = clients.get(tool_info.server_key()) else {
            if model_visible {
                tools.push(tool_info);
            }
            continue;
        };
        if config
            .permission_profile_for_server(tool_info.server_key())
            .is_none()
        {
            trace!(
                server_name = %tool_info.server_key(),
                tool_name = %tool_info.tool.name,
                "omitting MCP tool without an exact ready client"
            );
            continue;
        }
        let pending_tool_info = if model_visible {
            let tool_index = tools.len();
            tools.push(tool_info);
            PendingCallToolInfo::BindingCatalog(tool_index)
        } else {
            PendingCallToolInfo::Owned(tool_info.into())
        };
        pending_calls.push((pending_tool_info, Arc::clone(client), Arc::clone(snapshot)));
    }
    let tools: Arc<[ToolInfo]> = tools.into();
    let mut calls = HashMap::with_capacity(pending_calls.len());
    for (pending_tool_info, client, snapshot) in pending_calls {
        let tool_info = match pending_tool_info {
            PendingCallToolInfo::BindingCatalog(tool_index) => {
                PreparedToolInfo::from_binding_catalog(Arc::clone(&tools), tool_index)
            }
            PendingCallToolInfo::Owned(tool_info) => tool_info,
        };
        let Some(call) = connections.prepare_call(tool_info, client, Arc::clone(&config), snapshot)
        else {
            continue;
        };
        calls.insert(
            (
                call.server_key().clone(),
                call.tool_info().tool.name.to_string(),
            ),
            call,
        );
    }
    let deferred_servers = connections
        .servers
        .iter()
        .filter(|(_, view)| {
            view.connection.startup_is_dormant()
                && !view.connection.client.cancel_token.is_cancelled()
        })
        .map(|(name, _)| name.clone())
        .collect();
    let clients = Arc::new(McpBindingClients::new(
        clients
            .into_iter()
            .map(|(server_name, (client, _))| (server_name, client))
            .collect(),
        deferred_servers,
    ));
    McpBinding::new(
        Arc::clone(connections),
        clients,
        config,
        plugins_available,
        tools,
        calls,
    )
}
