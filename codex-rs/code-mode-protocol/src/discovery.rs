//! Discovery metadata retains output schemas for explicit lookup, not repeated prompts.

use std::sync::Arc;

use codex_protocol::ToolName;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::CodeModeToolKind;
use crate::ToolDefinition;
use crate::normalize_code_mode_identifier;

const MAX_SUMMARY_CHARACTERS: usize = 160;

pub fn enabled_tool_metadata(definition: &ToolDefinition) -> EnabledToolMetadata {
    EnabledToolMetadata {
        tool_name: definition.tool_name.clone(),
        global_name: normalize_code_mode_identifier(&definition.name),
        description: definition.description.clone(),
        summary: compact_tool_summary(&definition.summary),
        kind: definition.kind,
        input_schema: definition.input_schema.clone().map(Arc::new),
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
    input_schema: Option<Arc<JsonValue>>,
    #[serde(skip)]
    output_schema: Option<Arc<JsonValue>>,
}

impl EnabledToolMetadata {
    #[must_use]
    pub fn describe_output(&self) -> String {
        let Some(output) = self.output_schema.as_deref() else {
            return format!("{}: no declared output schema", self.global_name);
        };
        if let Some(structured) = mcp_structured_content_schema(Some(output)) {
            format!(
                "{} returns an MCP result: content blocks; optional structuredContent, isError, _meta.\n{}",
                self.global_name,
                crate::contract::schema("structuredContent", structured)
            )
        } else {
            crate::contract::schema(&format!("{} returns", self.global_name), output)
        }
    }

    #[must_use]
    pub fn describe_schema(&self) -> String {
        serde_json::json!({"input": self.input_schema, "output": self.output_schema}).to_string()
    }
}

/// Collapses whitespace and keeps the first purpose sentence; a sentence longer
/// than the budget is cut at a word boundary and marked with an ellipsis. A period followed by a
/// lowercase word, as in "e.g. this", does not end a sentence.
pub(crate) fn compact_tool_summary(purpose: &str) -> String {
    let mut summary = String::new();
    let mut characters = 0;
    let mut pending_end = false;
    for word in purpose.split_whitespace() {
        if pending_end && !word.starts_with(char::is_lowercase) {
            return summary;
        }
        pending_end = false;
        let width = usize::from(characters > 0) + word.chars().count();
        if characters + width > MAX_SUMMARY_CHARACTERS {
            // Keep whole words, leaving room for the ellipsis.
            if characters == MAX_SUMMARY_CHARACTERS {
                match summary.rfind(' ') {
                    Some(boundary) => summary.truncate(boundary),
                    None => {
                        let _ = summary.pop();
                    }
                }
            }
            if summary.is_empty() {
                summary.extend(word.chars().take(MAX_SUMMARY_CHARACTERS - 1));
            }
            summary.push('…');
            return summary;
        }
        if characters > 0 {
            summary.push(' ');
        }
        summary.push_str(word);
        characters += width;
        if word.ends_with(['.', '!', '?']) {
            pending_end = true;
        }
    }
    summary
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
