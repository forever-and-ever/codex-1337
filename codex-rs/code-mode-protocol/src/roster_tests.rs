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
        let entries = roster(&tools, dialect);
        let rendered = render_roster(&entries).unwrap();
        for tool in &tools {
            assert!(rendered.contains(&format!(
                "tools.{}",
                crate::normalize_code_mode_identifier(&tool.0.name)
            )));
            assert!(rendered.contains(&tool.0.description));
        }
        assert_eq!(rendered.trim().lines().count(), tools.len() + 1);
        // Only the Responses dialect has a namespace reference that differs from the member.
        assert_eq!(
            rendered.contains("mcp__depot.lsp.rename"),
            dialect == ToolDialect::Responses
        );
        assert_eq!(render_roster(&roster(&[], dialect)), None);
        // An amendment names what the model must add or forget, and nothing else.
        let current = roster_record(&entries);
        let mut told = current.clone();
        assert_eq!(render_roster_amendment(&current, &told), None);
        told.remove(&entries[0].name);
        told.insert("tools.retired".to_owned(), "Retired.".to_owned());
        assert_eq!(
            render_roster_amendment(&current, &told),
            Some(format!(
                "\nAdded or changed:\n- {}: {}\nRemoved:\n- tools.retired\n",
                entries[0].name, entries[0].purpose
            ))
        );
        assert_eq!(
            render_roster_amendment(&RosterRecord::new(), &told).as_deref(),
            Some("\nNo rostered tools remain.\n")
        );
    }
}
