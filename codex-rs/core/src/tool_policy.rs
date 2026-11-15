use crate::CodexThread;
use crate::config::Config;
use crate::mcp::McpRuntimeProjection;
use crate::tools::ToolRouter;
use crate::tools::registry::ToolRegistry;
use codex_extension_api::ExtensionDataInit;
use codex_extension_api::SelectedPluginSnapshot;
use codex_extension_api::ToolPolicy;
use codex_mcp::ResolvedMcpCatalog;
use codex_plugin::PluginLoadOutcome;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::SessionSource;
use codex_protocol::tool_policy::ToolPolicyCeiling;
use std::collections::BTreeMap;
use std::sync::Arc;

impl CodexThread {
    pub fn tool_policy_ceiling(&self) -> Option<ToolPolicyCeiling> {
        excludes_all(&self.session.tool_policy).then_some(ToolPolicyCeiling::NoTools)
    }
}

fn excludes_all(policy: &ToolPolicy) -> bool {
    policy.allowed_tools.as_ref().is_some_and(Vec::is_empty)
}

// TELOS(startup-tool-ceiling): Capture a ceiling once; later configuration or discovery cannot broaden it.

pub(crate) fn capture(init: &mut ExtensionDataInit, source: &SessionSource) -> Arc<ToolPolicy> {
    let inherited = init.get::<ToolPolicy>().unwrap_or_else(|| {
        // Older reviewer rollouts predate the explicit startup policy.
        if crate::guardian::is_basic_session_source(source) {
            Arc::new(codex_guardian_reviewer::reviewer_tool_policy())
        } else {
            Arc::default()
        }
    });
    let policy = match init.get::<ToolPolicyCeiling>().as_deref() {
        Some(ToolPolicyCeiling::NoTools) => Arc::new(ToolPolicy {
            allowed_tools: Some(Vec::new()),
            ..inherited.as_ref().clone()
        }),
        None => inherited,
    };
    // MCP receives this startup snapshot, never the mutable extension store.
    init.insert(policy.as_ref().clone());
    policy
}

pub(crate) fn empty_router(policy: Arc<ToolPolicy>) -> Option<ToolRouter> {
    if !excludes_all(&policy) {
        return None;
    }
    Some(ToolRouter::from_parts(
        ToolRegistry::with_tool_policy(policy),
        Vec::new(),
        ToolMode::Direct,
        BTreeMap::default(),
        None,
        None,
        &[],
    ))
}

pub(crate) fn empty_mcp_projection(
    config: &Config,
    init: &ExtensionDataInit,
) -> Option<McpRuntimeProjection> {
    let policy = init.get::<ToolPolicy>()?;
    if !excludes_all(&policy) {
        return None;
    }
    // No capability can use MCP: do not start even required servers or contributors.
    let mut config = config.to_mcp_config_with_loaded_plugins(&PluginLoadOutcome::default(), []);
    config.mcp_server_catalog = ResolvedMcpCatalog::default();
    config.apps_enabled = false;
    Some(McpRuntimeProjection {
        config,
        plugins_available: false,
        selected_plugins: SelectedPluginSnapshot::default(),
    })
}
