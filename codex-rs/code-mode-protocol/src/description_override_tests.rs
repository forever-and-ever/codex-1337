//! Model overrides do not suppress execution rules or tool existence.
use super::*;
use crate::presentation::ToolDialect;
use codex_protocol::openai_models::ToolMessage;
use serde_json::json;

const MEDIA: MediaHelpers = MediaHelpers {
    image_detail: ImageDetailVisibility::Visible,
    audio: false,
    generated_image: false,
};

#[test]
fn exec_override_renders_only_known_literal_placeholders() {
    for dialect in [
        ToolDialect::Responses,
        ToolDialect::Anthropic,
        ToolDialect::Other,
    ] {
        let description = build_exec_tool_description_for(
            dialect, &[], &[], &BTreeMap::new(), 4567, true, MEDIA,
            DeferredToolDiscovery::Catalog,
            Some(&CodeModeToolMessages {
                exec: Some(ToolMessage {
                    description: Some(" {{ max_yield_time_ms }} {{ image_helper }} {{ unknown }} {{max_yield_time_ms}} ".into()),
                    ..Default::default()
                }), ..Default::default()
            }),
        );
        assert!(description.starts_with(&format!(
            " 4567 {LEGACY_IMAGE_HELPER_DESCRIPTION} {{{{ unknown }}}} {{{{max_yield_time_ms}}}} "
        )));
        for required in [
            EXECUTION_ORDER_GUIDANCE,
            TOOL_DISCOVERY_GUIDANCE,
            EXECUTION_OUTPUT_GUIDANCE,
        ] {
            assert!(description.contains(required));
        }
        assert!(!description.contains("declare const tools"));
    }
}

#[test]
fn exec_override_preserves_runtime_sections() {
    let tool = ToolDefinition {
        name: "sample__echo".into(),
        tool_name: ToolName::namespaced("sample", "echo"),
        description: "Echo a value.".into(),
        summary: String::new(),
        kind: CodeModeToolKind::Function,
        input_schema: Some(
            json!({"type":"object", "properties":{"value":{"type":"string","minLength":1}},"required":["value"]}),
        ),
        output_schema: None,
    };
    let entries_for = |enabled: &[ToolDefinition], undeclared: &[UndeclaredTool]| {
        build_exec_tool_description_and_definitions_for(
            ToolDialect::Responses,
            enabled,
            undeclared,
            &BTreeMap::new(),
            600_000,
            true,
            MEDIA,
            DeferredToolDiscovery::Catalog,
            true,
            None,
        )
        .1
    };
    let initial = entries_for(&[], &[UndeclaredTool(tool.clone())]);
    let mut changed = tool.clone();
    changed.summary = "Changed purpose.".into();
    let updated = entries_for(&[], &[UndeclaredTool(changed.clone())]);
    assert_eq!(initial[0].key, updated[0].key);
    assert_ne!(initial[0].description, updated[0].description);
    assert!(
        entries_for(&[], &[]).is_empty(),
        "removed inventory keys must disappear"
    );
    let eager = entries_for(&[changed], &[]);
    assert!(
        eager
            .iter()
            .any(|entry| entry.key == "Tool: tools.sample__echo")
    );
    assert_eq!(initial[0].key, eager[0].key);
    assert_ne!(initial[0].description, eager[0].description);
    for dialect in [
        ToolDialect::Responses,
        ToolDialect::Anthropic,
        ToolDialect::Other,
    ] {
        for description_override in ["", " \n\t "] {
            let (description, entries) = build_exec_tool_description_and_definitions_for(
                dialect,
                &[],
                &[UndeclaredTool(tool.clone())],
                &BTreeMap::new(),
                600_000,
                true,
                MEDIA,
                DeferredToolDiscovery::Catalog,
                true,
                Some(&CodeModeToolMessages {
                    exec: Some(ToolMessage {
                        description: Some(description_override.into()),
                        ..Default::default()
                    }),
                    deferred_nested_tools_guidance: Some(String::new()),
                    mcp_typescript_preamble: Some("Caller-provided MCP guidance.".into()),
                    ..Default::default()
                }),
            );
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].key, "Tool: tools.sample__echo");
            assert!(description.contains(&entries[0].description));
            assert!(description.contains("tools.sample__echo"));
            assert!(description.contains("Echo a value."));
            assert!(
                !description.contains("minLength"),
                "inventory must not repeat a deferred contract"
            );
            assert_eq!(
                description.contains("sample.echo"),
                dialect == ToolDialect::Responses
            );
            assert!(description.contains(TOOL_DISCOVERY_GUIDANCE));
            assert!(description.contains("Caller-provided MCP guidance."));
        }
    }
}

#[test]
fn exec_header_is_stable_when_deferred_output_schema_changes() {
    let mut tool = ToolDefinition {
        name: "sample".into(),
        tool_name: ToolName::plain("sample"),
        description: "Read a value.".into(),
        summary: String::new(),
        kind: CodeModeToolKind::Function,
        input_schema: None,
        output_schema: None,
    };
    let render = |tool: &ToolDefinition| {
        build_exec_tool_description(
            &[],
            &[UndeclaredTool(tool.clone())],
            &BTreeMap::new(),
            600_000,
            true,
            MEDIA,
            DeferredToolDiscovery::Catalog,
            None,
        )
    };
    let before = render(&tool);
    tool.output_schema = Some(json!({"type":"object","properties":{"fresh":{"type":"boolean"}}}));
    assert_eq!(before, render(&tool));
    let metadata = crate::enabled_tool_metadata(augment_tool_definition(tool));
    assert!(
        metadata
            .describe_output()
            .contains("fresh (optional): boolean")
    );
    assert!(metadata.describe_schema().contains("\"fresh\""));
}
