use super::*;
use crate::CodeModeToolKind;
use codex_protocol::ToolName;

#[test]
fn roster_retains_every_callable_and_purpose_in_each_dialect() {
    let tools = ["sem.read.body", "sem.read.callers", "lsp.rename"].map(|name| {
        UndeclaredTool(ToolDefinition {
            name: format!("mcp__depot__{name}"),
            tool_name: ToolName::namespaced("mcp__depot", name),
            description: format!("Perform {name}."),
            summary: String::new(),
            kind: CodeModeToolKind::Function,
            input_schema: None,
            output_schema: None,
        })
    });
    for dialect in [
        ToolDialect::Responses,
        ToolDialect::Anthropic,
        ToolDialect::Other,
    ] {
        let roster = render_roster_for(&tools, dialect).unwrap();
        for tool in &tools {
            assert!(roster.contains(&format!(
                "tools.{}",
                crate::normalize_code_mode_identifier(&tool.0.name)
            )));
            assert!(roster.contains(&tool.0.description));
        }
        assert_eq!(roster.lines().count(), tools.len() + 1);
        assert_eq!(render_roster_for(&[], dialect), None);
    }
}
