use codex_code_mode::MediaHelpers;
use codex_code_mode::ToolDefinition as CodeModeToolDefinition;
use codex_code_mode::UndeclaredTool;
use codex_protocol::openai_models::CodeModeToolMessages;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub(crate) fn create_code_mode_tool(
    enabled_tools: &[CodeModeToolDefinition],
    undeclared_tools: &[UndeclaredTool],
    namespace_descriptions: &BTreeMap<String, codex_code_mode::ToolNamespaceDescription>,
    max_yield_time_ms: u64,
    code_mode_only: bool,
    media_helpers: MediaHelpers,
    messages: Option<&CodeModeToolMessages>,
) -> ToolSpec {
    const CODE_MODE_FREEFORM_GRAMMAR: &str = r#"
start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE

PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;

    ToolSpec::Freeform(FreeformTool {
        name: codex_code_mode::PUBLIC_TOOL_NAME.to_string(),
        description: codex_code_mode::build_exec_tool_description(
            enabled_tools,
            undeclared_tools,
            namespace_descriptions,
            max_yield_time_ms,
            code_mode_only,
            media_helpers,
            messages,
        ),
        defer_loading: None,
        format: FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: "lark".to_string(),
            definition: CODE_MODE_FREEFORM_GRAMMAR.to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_code_mode::ImageDetailVisibility;
    use codex_tools::ToolName;
    use pretty_assertions::assert_eq;

    #[test]
    fn create_code_mode_tool_matches_expected_spec() {
        let media_helpers = MediaHelpers {
            image_detail: ImageDetailVisibility::Visible,
            audio: false,
            generated_image: false,
        };
        let enabled_tools = vec![codex_code_mode::ToolDefinition {
            summary: String::new(),
            name: "update_plan".to_string(),
            tool_name: ToolName::plain("update_plan"),
            description: "Update the plan".to_string(),
            kind: codex_code_mode::CodeModeToolKind::Function,
            input_schema: None,
            input_schema_max_bytes: None,
            output_schema: None,
        }];

        assert_eq!(
            create_code_mode_tool(
                &enabled_tools,
                &[],
                &BTreeMap::new(),
                /*max_yield_time_ms*/ 600_000,
                /*code_mode_only*/ true,
                media_helpers,
                /*messages*/ None,
            ),
            ToolSpec::Freeform(FreeformTool {
                name: codex_code_mode::PUBLIC_TOOL_NAME.to_string(),
                description: codex_code_mode::build_exec_tool_description(
                    &enabled_tools,
                    &[],
                    &BTreeMap::new(),
                    /*max_yield_time_ms*/ 600_000,
                    /*code_mode_only*/ true,
                    media_helpers,
                    /*messages*/ None,
                ),
                defer_loading: None,
                format: FreeformToolFormat {
                    r#type: "grammar".to_string(),
                    syntax: "lark".to_string(),
                    definition: r#"
start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE

PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#
                    .to_string(),
                },
            })
        );
    }
}
