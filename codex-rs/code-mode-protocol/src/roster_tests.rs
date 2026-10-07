use super::HEADER;
use super::UndeclaredTool;
use super::render_roster;
use crate::CodeModeToolKind;
use crate::ToolDefinition;
use codex_protocol::ToolName;
use pretty_assertions::assert_eq;

#[test]
fn roster_factors_shared_name_prefixes_and_omits_deferred_tools() {
    let tool = |name: &str| ToolDefinition {
        summary: String::new(),
        name: name.to_string(),
        tool_name: ToolName::plain(name),
        description: String::new(),
        kind: CodeModeToolKind::Function,
        input_schema: None,
        input_schema_max_bytes: None,
        output_schema: None,
    };
    let mut tools = [
        "plain_tool",
        "mcp__depot__depot_health",
        "mcp__depot__lsp_hover",
        "mcp__depot__lsp_symbols",
        "mcp__depot__sem_edit_apply",
        "mcp__depot__sem_read_body",
        "mcp__depot__sem_read_callers",
        "mcp__x__echo",
        "mcp__x__echo_tool",
        "mcp-dash__y__z",
    ]
    .map(|name| UndeclaredTool::Rostered(tool(name)))
    .to_vec();
    tools.push(UndeclaredTool::Deferred(tool("mcp__depot__lsp_rename")));

    assert_eq!(
        render_roster(&tools),
        Some(format!(
            "{HEADER}\n\
             - plain_tool\n\
             - mcp__depot__depot_health\n\
             - mcp__depot__lsp_{{hover,symbols}}\n\
             - mcp__depot__sem_{{edit_apply,read_body,read_callers}}\n\
             - mcp__x__echo\n\
             - mcp__x__echo_tool\n\
             - mcp_dash__y__z"
        ))
    );
    assert_eq!(render_roster(&tools[tools.len() - 1..]), None);
}
