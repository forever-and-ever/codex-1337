use super::ToolRouter;

impl ToolRouter {
    pub(crate) fn declares_mcp_tools(&self) -> bool {
        self.registry.entries().any(|tool| {
            tool.runtime.mcp_server_name().is_some() && self.exposes_tool(&tool.runtime.tool_name())
        })
    }
}
