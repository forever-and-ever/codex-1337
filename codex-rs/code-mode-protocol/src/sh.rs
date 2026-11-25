//! `sh`, the Code Mode helper that runs commands through native `exec_command`.

use codex_protocol::ToolName;

use crate::ToolDefinition;

const HELPER: &str = "- `sh(...commands)`: run shell commands concurrently through `tools.exec_command`, each a `cmd` string or an arguments object, in Bash or a remote environment's reported shell. Appends their results in order, as `text(results)` would, with `Error` for a rejected call, and resolves to the results. Rejected calls and nonzero exits are data. The cell waits for `sh` even when it is not awaited.\n";

/// The nested tool `sh` runs commands through; a cell without it has no `sh`.
pub fn is_exec_command(tool_name: &ToolName) -> bool {
    tool_name.is_default_namespace() && tool_name.name == "exec_command"
}

/// The `exec` description's `sh` bullet, present only beside its tool.
pub(crate) fn helper<'a>(mut tools: impl Iterator<Item = &'a ToolDefinition>) -> &'static str {
    if tools.any(|tool| is_exec_command(&tool.tool_name)) {
        HELPER
    } else {
        ""
    }
}
