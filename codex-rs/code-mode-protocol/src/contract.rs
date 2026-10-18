//! One lossless documentation projection of the executable schemas.
use std::fmt::Write as _;

use std::collections::BTreeSet;

use serde_json::Value;

use crate::CodeModeToolKind;
use crate::ToolDefinition;
use crate::normalize_code_mode_identifier;

pub(crate) fn render(tool: &ToolDefinition) -> String {
    let mut text = tool.description.trim().to_owned();
    let _ = write!(
        text,
        "\n\nCall: await tools.{}(",
        normalize_code_mode_identifier(&tool.name)
    );
    text.push_str(match tool.kind {
        CodeModeToolKind::Function => "args).",
        CodeModeToolKind::Freeform => "input). Input is a raw string, not a JSON envelope.",
    });
    if let Some(input) = &tool.input_schema {
        text.push('\n');
        text.push_str(&schema("Parameters", input));
    }
    if let Some(output) = &tool.output_schema {
        if crate::discovery::mcp_structured_content_schema(Some(output)).is_some() {
            text.push_str("\nReturns: MCP result: content blocks; optional structuredContent, isError and _meta. Inspect isError; use describe_tool(name, {detail: \"output\"}) for the declared result fields.");
        } else {
            text.push('\n');
            text.push_str(&schema("Returns", output));
        }
    } else {
        text.push_str("\nReturns: no declared shape.");
    }
    text
}

pub(crate) fn schema(label: &str, root: &Value) -> String {
    let mut renderer = Renderer {
        lines: Vec::new(),
        references: BTreeSet::new(),
    };
    renderer.node(label, root, 0);
    while let Some(reference) = renderer
        .references
        .iter()
        .find(|name| {
            !renderer
                .lines
                .iter()
                .any(|line| line == &format!("{name}:"))
        })
        .cloned()
    {
        renderer.lines.push(format!("{reference}:"));
        match reference
            .strip_prefix('#')
            .and_then(|pointer| root.pointer(pointer))
        {
            Some(target) => renderer.node("value", target, 1),
            None => renderer.lines.push(
                "  Unresolved schema reference; inspect describe_tool(name, {detail: \"schema\"}) before constructing this value.".to_owned(),
            ),
        }
    }
    renderer.lines.join("\n")
}

struct Renderer {
    lines: Vec<String>,
    references: BTreeSet<String>,
}

impl Renderer {
    fn node(&mut self, name: &str, value: &Value, depth: usize) {
        let pad = "  ".repeat(depth);
        if let Some(allowed) = value.as_bool() {
            self.lines.push(format!(
                "{pad}{name}: {}",
                if allowed {
                    "any value"
                } else {
                    "no value allowed"
                }
            ));
            return;
        }
        let Some(object) = value.as_object() else {
            self.lines.push(format!("{pad}{name}: {value}"));
            return;
        };
        let kind = match object.get("type") {
            Some(Value::String(kind)) => kind.clone(),
            Some(Value::Array(kinds)) => kinds
                .iter()
                .map(|kind| kind.as_str().unwrap_or("unknown"))
                .collect::<Vec<_>>()
                .join(" or "),
            _ if object.contains_key("properties") => "object".to_owned(),
            _ => "value".to_owned(),
        };
        let mut line = format!("{pad}{name}: {kind}");
        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            let _ = write!(line, "; see {reference}");
            self.references.insert(reference.to_owned());
        }
        if let Some(description) = object.get("description").and_then(Value::as_str) {
            line.push_str(". ");
            line.push_str(description);
        }
        // Never drop a constraint merely because it has no specialized rendering.
        for (key, constraint) in object {
            if !matches!(
                key.as_str(),
                "type"
                    | "description"
                    | "properties"
                    | "required"
                    | "$ref"
                    | "$defs"
                    | "definitions"
                    | "items"
                    | "anyOf"
                    | "oneOf"
                    | "allOf"
                    | "additionalProperties"
                    | "title"
                    | "$schema"
                    | "$id"
            ) {
                let meaning = match key.as_str() {
                    "default" => "default",
                    "const" => "must equal",
                    "enum" => "allowed values",
                    "minimum" => "at least",
                    "maximum" => "at most",
                    "exclusiveMinimum" => "greater than",
                    "exclusiveMaximum" => "less than",
                    "multipleOf" => "multiple of",
                    "minLength" => "minimum characters",
                    "maxLength" => "maximum characters",
                    "pattern" => "must match pattern",
                    "format" => "format",
                    "minItems" => "minimum items",
                    "maxItems" => "maximum items",
                    "uniqueItems" => "unique items",
                    "minProperties" => "minimum fields",
                    "maxProperties" => "maximum fields",
                    _ => key,
                };
                let _ = write!(line, "; {meaning}: {constraint}");
            }
        }
        self.lines.push(line);
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            let properties = object.get("properties").and_then(Value::as_object);
            let extra = required
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| properties.is_none_or(|properties| !properties.contains_key(*name)))
                .collect::<Vec<_>>();
            if !extra.is_empty() {
                self.lines
                    .push(format!("{pad}  Required fields: {}", extra.join(", ")));
            }
        }
        if let Some(properties) = object.get("properties").and_then(Value::as_object) {
            let required = object.get("required").and_then(Value::as_array);
            for (key, property) in properties {
                let required = required.is_some_and(|required| {
                    required.iter().any(|value| value.as_str() == Some(key))
                });
                self.node(
                    &format!("{key} ({})", if required { "required" } else { "optional" }),
                    property,
                    depth + 1,
                );
            }
        }
        if let Some(items) = object.get("items") {
            self.node("each item", items, depth + 1);
        }
        for (key, label) in [
            ("oneOf", "exactly one"),
            ("anyOf", "at least one"),
            ("allOf", "all"),
        ] {
            if let Some(variants) = object.get(key).and_then(Value::as_array) {
                for (index, variant) in variants.iter().enumerate() {
                    self.node(
                        &format!("{label}, variant {}", index + 1),
                        variant,
                        depth + 1,
                    );
                }
            }
        }
        if let Some(extra) = object.get("additionalProperties") {
            match extra {
                Value::Bool(false) => self.lines.push(format!("{pad}  Other fields forbidden.")),
                Value::Bool(true) => {}
                schema => self.node("other fields", schema, depth + 1),
            }
        }
    }
}
