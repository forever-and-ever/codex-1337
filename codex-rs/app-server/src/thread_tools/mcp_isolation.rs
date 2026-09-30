//! Keep unrelated MCP registrations callable without sharing the native namespace.
//! Transport enablement and managed admission remain configuration-owned.
//! Historical registrations have no authentic frontend ownership marker (CLI
//! overrides share SessionFlags provenance). Retire them only through explicit
//! transport disablement, with native user policy configured independently.

use codex_core::config::Config;
use codex_extension_api::{
    ExtensionFuture, McpServerContribution, McpServerContributionContext, McpServerContributor,
};

pub(super) struct CoordinationMcpIsolation;

impl McpServerContributor<Config> for CoordinationMcpIsolation {
    fn id(&self) -> &'static str {
        "native_thread_coordination"
    }

    fn contribute<'a>(
        &'a self,
        _context: McpServerContributionContext<'a, Config>,
    ) -> ExtensionFuture<'a, Vec<McpServerContribution>> {
        Box::pin(async {
            vec![McpServerContribution::ForceToolNamespacePrefix {
                name: super::POLICY_NAMESPACE.into(),
            }]
        })
    }
}
