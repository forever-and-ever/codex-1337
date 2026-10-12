//! How `exec`'s description discloses each nested tool.

use codex_protocol::openai_models::ToolMode;
use codex_tools::TOOL_SEARCH_TOOL_NAME;
use codex_tools::ToolExposure;
use codex_tools::ToolName;

use crate::tools::registry::RegisteredTool;

/// Code Mode Only declares just the built-ins a session needs before it can discover anything
/// else: the shell, the editor, and ranked discovery. Every other contract waits for
/// `describe_tool`.
const BOOTSTRAP: [&str; 3] = ["exec_command", "apply_patch", TOOL_SEARCH_TOOL_NAME];

/// How `exec`'s description discloses a nested tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Disclosure {
    /// Declared in full.
    Declared,
    /// Rostered beside its purpose summary.
    Glossed,
}

impl Disclosure {
    pub(crate) fn of(
        tool: &RegisteredTool,
        tool_name: &ToolName,
        tool_mode: ToolMode,
        _ranked_search: bool,
    ) -> Self {
        if tool.exposure == ToolExposure::Deferred
            || (tool_mode == ToolMode::CodeModeOnly
                && (tool.runtime.mcp_server_name().is_some()
                    || (!tool.runtime.is_third_party_tool()
                        && !(tool_name.is_default_namespace()
                            && BOOTSTRAP.contains(&tool_name.name.as_str())))))
        {
            Self::Glossed
        } else {
            Self::Declared
        }
    }
}
