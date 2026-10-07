//! Tests catalog exec template rendering and runtime description composition.

use super::CodeModeToolKind;
use super::DEFERRED_NESTED_TOOLS_GUIDANCE;
use super::EXECUTION_OUTPUT_GUIDANCE;
use super::ImageDetailVisibility;
use super::LEGACY_IMAGE_HELPER_DESCRIPTION;
use super::MCP_TYPESCRIPT_PREAMBLE;
use super::MediaHelpers;
use super::TOOL_DISCOVERY_GUIDANCE;
use super::ToolDefinition;
use super::UNIFIED_IMAGE_HELPER_DESCRIPTION;
use super::UndeclaredTool;
use super::build_exec_tool_description;
use codex_protocol::ToolName;
use codex_protocol::openai_models::CodeModeToolMessages;
use codex_protocol::openai_models::ToolMessage;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;

const ALL_MEDIA: MediaHelpers = MediaHelpers {
    image_detail: ImageDetailVisibility::Visible,
    audio: true,
    generated_image: true,
};

#[test]
fn exec_override_renders_only_known_literal_placeholders() {
    for (image_detail, image_helper) in [
        (
            ImageDetailVisibility::Visible,
            LEGACY_IMAGE_HELPER_DESCRIPTION,
        ),
        (
            ImageDetailVisibility::Hidden,
            UNIFIED_IMAGE_HELPER_DESCRIPTION,
        ),
    ] {
        let description = build_exec_tool_description(
            &[],
            &[],
            &BTreeMap::new(),
            /*default_exec_yield_time_ms*/ 4567,
            /*code_mode_only*/ false,
            MediaHelpers {
                image_detail,
                ..ALL_MEDIA
            },
            Some(&CodeModeToolMessages {
                exec: Some(ToolMessage {
                    description: Some(" \nDefaults to 10000 ms. {{ default_exec_yield_time_ms }} ms.\n{{ image_helper }}\n{{ unknown }} {{default_exec_yield_time_ms}}\t ".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );

        assert_eq!(
            description,
            format!(
                " \nDefaults to 10000 ms. 4567 ms.\n{image_helper}\n{{{{ unknown }}}} {{{{default_exec_yield_time_ms}}}}\t \n\n{TOOL_DISCOVERY_GUIDANCE}\n\n{EXECUTION_OUTPUT_GUIDANCE}"
            ),
        );
    }
}

#[test]
fn exec_override_preserves_empty_and_whitespace_only_text() {
    for description_override in ["", " \n\t "] {
        assert_eq!(
            build_exec_tool_description(
                &[],
                &[],
                &BTreeMap::new(),
                crate::DEFAULT_EXEC_YIELD_TIME_MS,
                /*code_mode_only*/ true,
                ALL_MEDIA,
                Some(&CodeModeToolMessages {
                    exec: Some(ToolMessage {
                        description: Some(description_override.to_string()),
                        ..Default::default()
                    }),
                    deferred_nested_tools_guidance: Some(
                        "No deferred tools; omit this.".to_string()
                    ),
                    mcp_typescript_preamble: Some("No MCP tools; omit this.".to_string()),
                    ..Default::default()
                }),
            ),
            if description_override.is_empty() {
                format!("{TOOL_DISCOVERY_GUIDANCE}\n\n{EXECUTION_OUTPUT_GUIDANCE}")
            } else {
                format!(
                    "{description_override}\n\n{TOOL_DISCOVERY_GUIDANCE}\n\n{EXECUTION_OUTPUT_GUIDANCE}"
                )
            },
        );
    }
}

#[test]
fn exec_override_preserves_runtime_sections() {
    let enabled_tools = [ToolDefinition {
        summary: String::new(),
        name: "alpha".to_string(),
        tool_name: ToolName::plain("alpha"),
        description: "First tool".to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: None,
        input_schema_max_bytes: None,
        output_schema: None,
    }];
    let deferred_tools = [UndeclaredTool::Deferred(ToolDefinition {
        summary: String::new(),
        name: "mcp__sample__beta".to_string(),
        tool_name: ToolName::namespaced("mcp__sample__", "beta"),
        description: "Deferred tool".to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: None,
        input_schema_max_bytes: None,
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "content": { "type": "array", "items": { "type": "object" } },
                "isError": { "type": "boolean" },
                "_meta": { "type": "object" }
            }
        })),
    })];
    let declaration = "### `alpha`\nFirst tool\n\nexec tool declaration:\n```ts\ndeclare const tools: { alpha(args: unknown): Promise<unknown>; };\n```";
    for (code_mode_only, guidance, preamble, expected) in [
        (true, Some(""), Some(""), declaration.to_string()),
        (
            true,
            None,
            Some(""),
            format!("{DEFERRED_NESTED_TOOLS_GUIDANCE}\n\n{declaration}"),
        ),
        (
            true,
            Some(""),
            None,
            format!("Shared MCP Types:\n```ts\n{MCP_TYPESCRIPT_PREAMBLE}\n```\n\n{declaration}"),
        ),
        (
            true,
            Some("  {{ image_helper }}\n"),
            Some("  {{ default_exec_yield_time_ms }}\n"),
            format!(
                "  {{{{ image_helper }}}}\n\n\nShared MCP Types:\n```ts\n  {{{{ default_exec_yield_time_ms }}}}\n\n```\n\n{declaration}"
            ),
        ),
        (
            false,
            Some("Catalog discovery."),
            Some("No types outside Code Mode Only."),
            "Catalog discovery.".to_string(),
        ),
    ] {
        assert_eq!(
            build_exec_tool_description(
                &enabled_tools,
                &deferred_tools,
                &BTreeMap::new(),
                crate::DEFAULT_EXEC_YIELD_TIME_MS,
                code_mode_only,
                ALL_MEDIA,
                Some(&CodeModeToolMessages {
                    exec: Some(ToolMessage {
                        description: Some(String::new()),
                        ..Default::default()
                    }),
                    deferred_nested_tools_guidance: guidance.map(str::to_string),
                    mcp_typescript_preamble: preamble.map(str::to_string),
                    ..Default::default()
                }),
            ),
            format!("{TOOL_DISCOVERY_GUIDANCE}\n\n{EXECUTION_OUTPUT_GUIDANCE}\n\n{expected}"),
        );
    }
}
