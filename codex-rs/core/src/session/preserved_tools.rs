//! Provenance captured before a prediction fork disables its MCP runtime.

use super::Session;
use crate::tools::router::ToolRouter;

/// The live parent declared at least one MCP-sourced tool when this fork was captured.
pub struct ParentMcpToolsDeclared;

struct LatestMcpDeclarations(bool);

impl Session {
    pub(super) fn capture_mcp_declarations(&self, router: &ToolRouter) {
        let _ = self
            .services
            .session_extension_data
            .insert(LatestMcpDeclarations(router.declares_mcp_tools()));
    }

    pub(crate) fn latest_router_declares_mcp_tools(&self) -> bool {
        self.services
            .session_extension_data
            .get::<LatestMcpDeclarations>()
            .is_some_and(|declarations| declarations.0)
    }

    pub(super) fn parent_mcp_tools_declared(&self) -> bool {
        self.services
            .thread_extension_data
            .get::<ParentMcpToolsDeclared>()
            .is_some()
    }
}
