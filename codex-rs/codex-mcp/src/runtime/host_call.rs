use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use super::McpDispatchLeaseGuard;
use super::McpRuntime;
use super::McpToolCallResult;
use super::McpToolStartup;
use super::catalog_boundary;
use crate::McpServerKey;

pub(super) async fn call_tool(
    runtime: &McpRuntime,
    server: &str,
    tool: &str,
    environment_id: Option<&str>,
    arguments: Option<serde_json::Value>,
    meta: Option<serde_json::Value>,
    requested_timeout: Option<Duration>,
    startup: McpToolStartup,
) -> anyhow::Result<McpToolCallResult> {
    let current = runtime.current.load_full();
    let server = current
        .connections
        .resolve_tool_call_server(server, tool, environment_id)?;
    let guard = dispatch_guard(runtime, &current, server)?;
    if startup == McpToolStartup::SkipDormantReplCleanup
        && current.connections.is_dormant_private_stdio(server)
    {
        return Ok(McpToolCallResult {
            result: super::CallToolResult {
                content: Vec::new(),
                structured_content: None,
                is_error: None,
                meta: None,
            },
            source_tool_namespace: None,
        });
    }
    let call = async {
        let attribution = if current.connections.wait_for_server_startup(server).await
            && let Some(revisions) = crate::connection_manager::boundary::catalog_revisions(
                &current.connections,
                /*required_servers*/ &[],
                /*required_plugins*/ &HashSet::new(),
                crate::client_tool_catalog::CatalogReadMode::Current,
            )
            .await
        {
            catalog_boundary::binding_from_published_runtime(
                Arc::clone(&current),
                /*required_servers*/ &[],
                /*required_plugins*/ &HashSet::new(),
                crate::client_tool_catalog::CatalogReadMode::Current,
            )
            .await
            .map(|binding| (revisions, binding))
        } else {
            current
                .cached_binding
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(|cached| {
                    Some((cached.catalog_revisions.clone(), cached.binding.upgrade()?))
                })
        };
        if !guard.is_valid(server) {
            anyhow::bail!("extension MCP lease revoked");
        }
        let result = Box::pin(current.connections.call_tool(
            server,
            tool,
            arguments,
            meta,
            requested_timeout,
            /*wait_for_server*/ true,
        ))
        .await?;
        // Host calls use live connections, unlike model calls pinned to a binding.
        // Check after dispatch so a refresh before or during the call invalidates attribution.
        // Revisions include client identity and increase on catalog changes. Reading them
        // does not start servers or refresh catalogs; all catalogs matter for name collisions.
        let namespace = if let Some((revisions, binding)) = attribution
            && crate::connection_manager::boundary::catalog_revisions(
                &current.connections,
                /*required_servers*/ &[],
                /*required_plugins*/ &HashSet::new(),
                crate::client_tool_catalog::CatalogReadMode::Current,
            )
            .await
            .as_ref()
                == Some(&revisions)
        {
            binding
                .tool_info(server, tool)
                .map(|info| info.callable_namespace.clone())
        } else {
            None
        };
        if !guard.is_valid(server) {
            anyhow::bail!("extension MCP lease revoked");
        }
        Ok(McpToolCallResult {
            result,
            source_tool_namespace: namespace,
        })
    };
    if let Some(cancellation) = guard.cancellation() {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => anyhow::bail!("extension MCP lease revoked"),
            result = call => result,
        }
    } else {
        call.await
    }
}

fn dispatch_guard(
    runtime: &McpRuntime,
    current: &Arc<super::PublishedMcpRuntime>,
    server: &McpServerKey,
) -> anyhow::Result<McpDispatchLeaseGuard> {
    let expected = {
        let leases = runtime
            .server_leases
            .lock()
            .map_err(|_| anyhow::anyhow!("MCP server lease authority poisoned"))?;
        let lease = leases.get(server);
        if lease.is_some_and(|lease| !lease.authorizes(current)) {
            anyhow::bail!("extension MCP lease is not ready");
        }
        lease.map(|lease| Arc::clone(&lease.cancellation))
    };
    let guard = McpDispatchLeaseGuard {
        leases: Arc::clone(&runtime.server_leases),
        expected,
    };
    if !guard.is_valid(server) {
        anyhow::bail!("extension MCP lease revoked");
    }
    Ok(guard)
}

pub(super) async fn read_resource(
    runtime: &McpRuntime,
    server: &str,
    environment_id: Option<&str>,
    params: super::ReadResourceRequestParams,
) -> anyhow::Result<super::ReadResourceResult> {
    let current = runtime.current.load_full();
    let server = &current
        .config
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("unknown MCP server '{server}'"))?
        .mcp_server_catalog
        .resolve_server_key(server, environment_id);
    let guard = dispatch_guard(runtime, &current, server)?;
    let read = async {
        let result = Box::pin(current.connections.read_resource(server, params)).await?;
        if !guard.is_valid(server) {
            anyhow::bail!("extension MCP lease revoked");
        }
        Ok(result)
    };
    if let Some(cancellation) = guard.cancellation() {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => anyhow::bail!("extension MCP lease revoked"),
            result = read => result,
        }
    } else {
        read.await
    }
}

// Direct host calls retain lease authority without capturing an attribution catalog.
pub(super) async fn call_tool_without_attribution(
    runtime: &McpRuntime,
    server: &str,
    environment_id: Option<&str>,
    tool: &str,
    arguments: Option<serde_json::Value>,
    meta: Option<serde_json::Value>,
) -> anyhow::Result<super::CallToolResult> {
    let current = runtime.current.load_full();
    let server = &current
        .config
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("unknown MCP server '{server}'"))?
        .mcp_server_catalog
        .resolve_server_key(server, environment_id);
    let guard = dispatch_guard(runtime, &current, server)?;
    let call = async {
        current.connections.validate_tool_call(server, tool)?;
        current.connections.wait_for_server_startup(server).await;
        if !guard.is_valid(server) {
            anyhow::bail!("extension MCP lease revoked");
        }
        let result = Box::pin(
            current
                .connections
                .call_tool(server, tool, arguments, meta, None, true),
        )
        .await?;
        if !guard.is_valid(server) {
            anyhow::bail!("extension MCP lease revoked");
        }
        Ok(result)
    };
    if let Some(cancellation) = guard.cancellation() {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => anyhow::bail!("extension MCP lease revoked"),
            result = call => result,
        }
    } else {
        call.await
    }
}
