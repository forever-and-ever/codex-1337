//! The roster of nested tools a tool plan presents to the model.

use super::ToolRouter;
use codex_code_mode::RosterEntry;
use std::sync::Arc;

impl ToolRouter {
    pub(crate) fn with_nested_tools(mut self, nested_tools: Vec<RosterEntry>) -> Self {
        self.nested_tools = nested_tools.into();
        self
    }

    pub(crate) fn nested_tools(&self) -> Arc<[RosterEntry]> {
        Arc::clone(&self.nested_tools)
    }
}
