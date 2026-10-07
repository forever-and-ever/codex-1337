use super::exec_sections;
use codex_code_mode::CodeModeToolKind;
use codex_code_mode::ImageDetailVisibility;
use codex_code_mode::MediaHelpers;
use codex_code_mode::ToolDefinition;
use codex_tools::ToolName;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

/// The census parses headings that `build_exec_tool_description` renders in another crate.
#[test]
fn exec_sections_split_the_rendered_nested_tools_losslessly() {
    let tool = |name: &str| ToolDefinition {
        summary: String::new(),
        name: name.to_string(),
        tool_name: ToolName::plain(name),
        description: format!("{name} does work.\n\n### Not a heading"),
        kind: CodeModeToolKind::Function,
        input_schema: None,
        input_schema_max_bytes: None,
        output_schema: None,
    };
    let description = codex_code_mode::build_exec_tool_description(
        &[tool("update_plan"), tool("mcp__depot__consult")],
        &[],
        &BTreeMap::new(),
        codex_code_mode::DEFAULT_EXEC_YIELD_TIME_MS,
        /*code_mode_only*/ true,
        MediaHelpers {
            image_detail: ImageDetailVisibility::Visible,
            audio: true,
            generated_image: true,
        },
        /*messages*/ None,
    );
    let (guidance, sections) = exec_sections(&description);
    assert_eq!(
        sections.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        vec!["update_plan", "mcp__depot__consult"]
    );
    let rejoined: String = std::iter::once(guidance)
        .chain(sections.iter().map(|(_, text)| *text))
        .collect();
    assert_eq!(rejoined, description);
}
