//! `sh`, the Code Mode helper that runs commands through native `exec_command`.

use codex_protocol::ToolName;

use crate::ToolDefinition;

const HELPER: &str = "- `sh(...commands)`: runs Bash commands concurrently through `tools.exec_command` (or the selected environment's reported shell), each a `cmd` string or its arguments object; appends their results in order like `text(results)`, with `Error` for a rejected call; resolves to the results. The script waits for `sh` even unawaited.\n";

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
