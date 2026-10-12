use super::FACTORING_LEGEND;
use super::HEADER;
use super::UndeclaredTool;
use super::render_roster;
use crate::CodeModeToolKind;
use crate::ToolDefinition;
use codex_protocol::ToolName;
use pretty_assertions::assert_eq;

#[test]
fn roster_glosses_built_in_tools_factors_named_tools_and_omits_deferred_tools() {
    let tool = |name: &str, description: &str| ToolDefinition {
        summary: String::new(),
        name: name.to_string(),
        tool_name: ToolName::plain(name),
        description: description.to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: None,
        input_schema_max_bytes: None,
        output_schema: None,
    };
    let glossed = [
        (
            "wrangler__spawn_agent",
            "Spawn an agent for a well-scoped task, e.g. a review.\nIt inherits your tools and sandbox. Pick a model override only when the user explicitly asks for one; the parent model is the default.",
        ),
        (
            "web__run",
            "Search the web and open pages, then find text in them, take screenshots of PDF pages, and look up finance quotes, weather forecasts, sports schedules, standings, and the time for any UTC offset.",
        ),
        ("get_goal", ""),
    ]
    .map(|(name, description)| UndeclaredTool::Glossed(tool(name, description)));
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
    .map(|name| UndeclaredTool::Named(tool(name, "Never rendered.")))
    .to_vec();
    tools.extend(glossed.iter().cloned());
    tools.push(UndeclaredTool::Deferred(tool("mcp__depot__lsp_rename", "")));

    let glosses = "\
        - get_goal\n\
        - web__run: Search the web and open pages, then find text in them, take screenshots of PDF pages, and look up finance quotes, weather forecasts, sports schedules,…\n\
        - wrangler__spawn_agent: Spawn an agent for a well-scoped task, e.g. a review. It inherits your tools and sandbox.";
    assert_eq!(
        render_roster(&tools),
        Some(format!(
            "{HEADER} {FACTORING_LEGEND}\n\
             {glosses}\n\
             - plain_tool\n\
             - mcp__depot__depot_health\n\
             - mcp__depot__lsp_{{hover,symbols}}\n\
             - mcp__depot__sem_{{edit_apply,read_body,read_callers}}\n\
             - mcp__x__echo\n\
             - mcp__x__echo_tool\n\
             - mcp_dash__y__z"
        ))
    );
    assert_eq!(
        render_roster(&glossed),
        Some(format!("{HEADER}\n{glosses}"))
    );
    assert_eq!(render_roster(&tools[tools.len() - 1..]), None);
}
