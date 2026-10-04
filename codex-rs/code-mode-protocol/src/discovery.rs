//! Discovery metadata retains output schemas for explicit lookup, not repeated prompts.

use std::collections::BTreeSet;
use std::sync::Arc;

use codex_protocol::ToolName;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::CodeModeToolKind;
use crate::ToolDefinition;
use crate::json_schema_types::DEFAULT_INPUT_SCHEMA_MAX_BYTES;
use crate::json_schema_types::RenderedJsonSchema;
use crate::json_schema_types::render_json_schema_to_typescript_with_budget;
use crate::normalize_code_mode_identifier;

const MAX_SUMMARY_CHARACTERS: usize = 160;
pub(crate) const COMPACT_OUTPUT_SCHEMA_MAX_BYTES: usize = 1_024;

pub fn enabled_tool_metadata(definition: &ToolDefinition) -> EnabledToolMetadata {
    EnabledToolMetadata {
        tool_name: definition.tool_name.clone(),
        global_name: normalize_code_mode_identifier(&definition.name),
        description: definition.description.clone(),
        summary: compact_tool_summary(&definition.summary),
        kind: definition.kind,
        output_schema: definition.output_schema.clone().map(Arc::new),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EnabledToolMetadata {
    pub tool_name: ToolName,
    pub global_name: String,
    pub description: String,
    pub summary: String,
    pub kind: CodeModeToolKind,
    #[serde(skip)]
    output_schema: Option<Arc<JsonValue>>,
}

impl EnabledToolMetadata {
    pub fn describe_output(&self) -> String {
        let Some(schema) = self.output_schema.as_deref() else {
            return format!("{}: no declared output schema", self.global_name);
        };
        let output =
            render_output_type(schema, DEFAULT_INPUT_SCHEMA_MAX_BYTES, &mut BTreeSet::new());
        format!(
            "{} output:\n```ts\n{}type ToolOutput = {};\n```",
            self.global_name, output.declarations, output.root_type
        )
    }
}

pub(crate) fn compact_tool_summary(purpose: &str) -> String {
    let mut characters = purpose
        .split_whitespace()
        .enumerate()
        .flat_map(|(index, word)| (index > 0).then_some(' ').into_iter().chain(word.chars()));
    let mut prefix = characters
        .by_ref()
        .take(MAX_SUMMARY_CHARACTERS)
        .collect::<String>();
    if characters.next().is_none() {
        return prefix;
    }
    prefix.pop();
    if let Some(boundary) = prefix.rfind(' ') {
        prefix.truncate(boundary);
    }
    prefix.push('…');
    prefix
}

pub(crate) fn render_output_type(
    schema: &JsonValue,
    max_bytes: usize,
    names: &mut BTreeSet<String>,
) -> RenderedJsonSchema {
    if let Some(structured_content_schema) = mcp_structured_content_schema(Some(schema)) {
        let mut output = render_json_schema_to_typescript_with_budget(
            structured_content_schema,
            max_bytes,
            names,
        );
        output.root_type = if output.root_type == "unknown" {
            "CallToolResult".to_string()
        } else {
            format!("CallToolResult<{}>", output.root_type)
        };
        output
    } else {
        render_json_schema_to_typescript_with_budget(schema, max_bytes, names)
    }
}

pub(crate) fn mcp_structured_content_schema(
    output_schema: Option<&JsonValue>,
) -> Option<&JsonValue> {
    let properties = output_schema?.get("properties")?.as_object()?;
    let content_schema = properties.get("content")?.as_object()?;
    if content_schema.get("type").and_then(JsonValue::as_str) != Some("array")
        || content_schema
            .get("items")
            .and_then(JsonValue::as_object)
            .is_none_or(|items| items.get("type").and_then(JsonValue::as_str) != Some("object"))
        || properties
            .get("isError")
            .and_then(JsonValue::as_object)
            .is_none_or(|schema| schema.get("type").and_then(JsonValue::as_str) != Some("boolean"))
        || properties
            .get("_meta")
            .and_then(JsonValue::as_object)
            .is_none_or(|schema| schema.get("type").and_then(JsonValue::as_str) != Some("object"))
    {
        return None;
    }
    Some(
        properties
            .get("structuredContent")
            .unwrap_or(&JsonValue::Bool(true)),
    )
}
