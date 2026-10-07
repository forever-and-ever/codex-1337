//! Composes Code Mode descriptions, retaining runtime-owned tool declarations.
//! Exec templates substitute only the documented literal placeholders.

use codex_protocol::ToolName;
use codex_protocol::openai_models::CodeModeToolMessages;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

use crate::PUBLIC_TOOL_NAME;
use crate::UndeclaredTool;
use crate::discovery::COMPACT_OUTPUT_SCHEMA_MAX_BYTES;
use crate::discovery::compact_tool_summary;
use crate::discovery::mcp_structured_content_schema;
use crate::discovery::render_output_type;
use crate::json_schema_types::DEFAULT_INPUT_SCHEMA_MAX_BYTES;
use crate::json_schema_types::RenderedJsonSchema;
use crate::json_schema_types::render_json_schema_to_typescript_with_budget;
use std::collections::BTreeSet;

const MAX_JS_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const DEFERRED_NESTED_TOOLS_GUIDANCE: &str = "Some deferred nested tools may be omitted from this description. They are still available on the global `tools` object and listed in `ALL_TOOLS`.";
const TOOL_DISCOVERY_GUIDANCE: &str = r#"Tool discovery helpers (synchronous):
- `ALL_TOOLS`: `{ name, description }` catalogue; exact callable names and purpose summaries, not signatures or full instructions.
- `search_tools(query: string, options?: { namespace?: string, limit?: number, offset?: number })`: returns `{ matches: Array<{ name, description }>, truncated: boolean, next_offset: number | null }`. All case-insensitive literal query words must match a name or summary; an empty query browses. `namespace` restricts the normalized namespace or callable-name prefix (`functions` for plain tools). Limit defaults to 8, capped at 20. Continue with the same query/options and `offset: next_offset`.
- `describe_tool(name: string, options?: { detail?: "contract" | "output" })`: defaults to full instructions, namespace guidance, input declaration, and a compact return type. Retrieve this contract when missing from context. `{detail: "output"}` returns only the detailed output type; request it when inspecting result fields. MCP contracts use the shared `CallToolResult` envelope. Other return types over 1 KiB are abbreviated; explicit output declarations retain the 16,000-byte schema limit.
Print bounded search results, not the whole catalogue:
```js
text(search_tools("symbol", { namespace: "mcp__depot", limit: 5 }));
text(describe_tool("mcp__depot__lsp_symbols"));
```"#;
const EXECUTION_OUTPUT_GUIDANCE: &str = r#"Execution output rules (override description templates):
- `text(value, options?: {metadata?: boolean})` and `notify(value, options?: {metadata?: boolean})` print compatible directly returned native `exec_command`/`write_stdin` objects as literal output plus necessary execution state. Nonempty dense arrays containing only such objects get ordinal headers; only default mode coalesces consecutive empty successful entries. `{metadata: true}` includes each result's metadata. JavaScript result fields are unchanged.
- `text(result)` is compact. Strings, including `result.output` and explicit `JSON.stringify(result)`, remain literal. All other values retain generic serialization."#;
const LEGACY_IMAGE_HELPER_DESCRIPTION: &str = r#"`image(imageUrlOrItem: string | { image_url: string; detail?: "auto" | "low" | "high" | "original" | null } | ImageContent, detail?: "auto" | "low" | "high" | "original" | null)`: Appends an image item. `image_url` should be a base64-encoded `data:` URL. To forward an MCP tool image, pass an individual `ImageContent` block from `result.content`, for example `image(result.content[0])`. MCP image blocks may request detail with `_meta: { "codex/imageDetail": "original" }`. When provided, the second `detail` argument overrides any detail embedded in the first argument."#;
const UNIFIED_IMAGE_HELPER_DESCRIPTION: &str = r#"`image(imageUrlOrItem: string | { image_url: string } | ImageContent)`: Appends an image item. `image_url` should be a base64-encoded `data:` URL. To forward an MCP tool image, pass an individual `ImageContent` block from `result.content`, for example `image(result.content[0])`."#;
const EXEC_DESCRIPTION_TEMPLATE: &str = r#"Run JavaScript code to orchestrate/compose tool calls
- Evaluates the provided JavaScript code in a fresh V8 isolate as an async module.
- All nested tools are available on the global `tools` object, for example `await tools.exec_command(...)`. Tool names are exposed as normalized JavaScript identifiers, for example `await tools.mcp__ologs__get_profile(...)`.
- Nested tool methods take either a string or an object as their input argument.
- Nested tools return either an object or a string, based on the description.
- Runs raw JavaScript -- no Node, no file system, no network access, no console.
- Accepts raw JavaScript source text, not JSON, quoted strings, or markdown code fences.
- You may optionally start the tool input with a first-line pragma like `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}`.
- `yield_time_ms` asks `exec` to yield early if the script is still running. Defaults to {{ default_exec_yield_time_ms }} ms.
- `max_output_tokens` sets the token budget for direct `exec` results. Defaults to 10000 tokens.
- When the JS code is fully evaluated, the isolate's lifetime ends and unawaited promises are silently discarded.

- Global helpers:
- `exit()`: Immediately ends the current script successfully (like an early return from the top level).
- `text(value, options?: {metadata?: boolean})`: Appends a text item; see Execution output rules below for native execution results and generic serialization.
- {{ image_helper }}
- `audio(audioUrlOrItem: string | { audio_url: string } | AudioContent)`: Appends an audio item. `audio_url` should be a base64-encoded `data:` URL. To forward an MCP tool audio block, pass an individual `AudioContent` block from `result.content`, for example `audio(result.content[0])`.
- `generatedImage(result: { image_url: string; output_hint?: string })`: Appends an image-generation result and its optional output hint. HTTP(S) URLs are not supported.
- `store(key: string, value: any)`: stores a serializable value under a string key for later `exec` calls in the same session.
- `load(key: string)`: returns the stored value for a string key, or `undefined` if it is missing.
- `notify(value, options?: {metadata?: boolean})`: immediately injects an extra `custom_tool_call_output` for the current `exec` call. Values are rendered like `text(...)`.
- `setTimeout(callback: () => void, delayMs?: number)`: schedules a callback to run later and returns a timeout id. Pending timeouts do not keep `exec` alive by themselves; await an explicit promise if you need to wait for one.
- `clearTimeout(timeoutId?: number)`: cancels a timeout created by `setTimeout`.
- `yield_control()`: yields the accumulated output to the model immediately while the script keeps running."#;
const WAIT_DESCRIPTION_TEMPLATE: &str = r#"- Use `wait` only after `exec` returns `Script running with cell ID ...`.
- `cell_id` identifies the running `exec` cell to resume.
- `yield_time_ms` controls how long to wait for more output before yielding again. Defaults to 10000 ms.
- `max_tokens` limits how much new output this wait call returns. Defaults to 10000 tokens.
- `terminate: true` stops the running cell; false or omitted waits for output.
- `wait` returns only the new output since the last yield, or the final completion or termination result for that cell.
- If the cell is still running, `wait` may yield again with the same `cell_id`.
- If the cell has already finished, `wait` returns the completed result and closes the cell."#;
// Based off of https://modelcontextprotocol.io/specification/draft/schema#calltoolresult
const MCP_TYPESCRIPT_PREAMBLE: &str = r#"type Role = "user" | "assistant";
type MetaObject = Record<string, unknown>;
type Annotations = {
  audience?: Role[];
  priority?: number;
  lastModified?: string;
};
type Icon = {
  src: string;
  mimeType?: string;
  sizes?: string[];
  theme?: "light" | "dark";
};
type TextResourceContents = {
  uri: string;
  mimeType?: string;
  _meta?: MetaObject;
  text: string;
};
type BlobResourceContents = {
  uri: string;
  mimeType?: string;
  _meta?: MetaObject;
  blob: string;
};
type TextContent = {
  type: "text";
  text: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ImageContent = {
  type: "image";
  data: string;
  mimeType: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type AudioContent = {
  type: "audio";
  data: string;
  mimeType: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ResourceLink = {
  icons?: Icon[];
  name: string;
  title?: string;
  uri: string;
  description?: string;
  mimeType?: string;
  annotations?: Annotations;
  size?: number;
  _meta?: MetaObject;
  type: "resource_link";
};
type EmbeddedResource = {
  type: "resource";
  resource: TextResourceContents | BlobResourceContents;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ContentBlock =
  | TextContent
  | ImageContent
  | AudioContent
  | ResourceLink
  | EmbeddedResource;
type CallToolResult<TStructured = { [key: string]: unknown }> = {
  _meta?: MetaObject;
  content: ContentBlock[];
  isError?: boolean;
  structuredContent?: TStructured;
  [key: string]: unknown;
};"#;

pub const CODE_MODE_PRAGMA_PREFIX: &str = "// @exec:";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeModeToolKind {
    Function,
    Freeform,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub tool_name: ToolName,
    pub description: String,
    #[serde(default)]
    pub summary: String,
    pub kind: CodeModeToolKind,
    pub input_schema: Option<JsonValue>,
    /// Internal budget used while rendering the input declaration, before sending it to the host.
    #[serde(skip)]
    pub input_schema_max_bytes: Option<usize>,
    pub output_schema: Option<JsonValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolNamespaceDescription {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CodeModeExecPragma {
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedExecSource {
    pub code: String,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<usize>,
}

pub fn parse_exec_source(input: &str) -> Result<ParsedExecSource, String> {
    if input.trim().is_empty() {
        return Err(
            "exec expects raw JavaScript source text (non-empty). Provide JS only, optionally with first-line `// @exec: {\"yield_time_ms\": 10000, \"max_output_tokens\": 1000}`.".to_string(),
        );
    }

    let mut args = ParsedExecSource {
        code: input.to_string(),
        yield_time_ms: None,
        max_output_tokens: None,
    };

    let mut lines = input.splitn(2, '\n');
    let first_line = lines.next().unwrap_or_default();
    let rest = lines.next().unwrap_or_default();
    let trimmed = first_line.trim_start();
    let Some(pragma) = trimmed.strip_prefix(CODE_MODE_PRAGMA_PREFIX) else {
        return Ok(args);
    };

    if rest.trim().is_empty() {
        return Err(
            "exec pragma must be followed by JavaScript source on subsequent lines".to_string(),
        );
    }

    let directive = pragma.trim();
    if directive.is_empty() {
        return Err(
            "exec pragma must be a JSON object with supported fields `yield_time_ms` and `max_output_tokens`"
                .to_string(),
        );
    }

    let value: serde_json::Value = serde_json::from_str(directive).map_err(|err| {
        format!(
            "exec pragma must be valid JSON with supported fields `yield_time_ms` and `max_output_tokens`: {err}"
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        "exec pragma must be a JSON object with supported fields `yield_time_ms` and `max_output_tokens`"
            .to_string()
    })?;
    for key in object.keys() {
        match key.as_str() {
            "yield_time_ms" | "max_output_tokens" => {}
            _ => {
                return Err(format!(
                    "exec pragma only supports `yield_time_ms` and `max_output_tokens`; got `{key}`"
                ));
            }
        }
    }

    let pragma: CodeModeExecPragma = serde_json::from_value(value).map_err(|err| {
        format!(
            "exec pragma fields `yield_time_ms` and `max_output_tokens` must be non-negative safe integers: {err}"
        )
    })?;
    if pragma
        .yield_time_ms
        .is_some_and(|yield_time_ms| yield_time_ms > MAX_JS_SAFE_INTEGER)
    {
        return Err(
            "exec pragma field `yield_time_ms` must be a non-negative safe integer".to_string(),
        );
    }
    if pragma.max_output_tokens.is_some_and(|max_output_tokens| {
        u64::try_from(max_output_tokens)
            .map(|max_output_tokens| max_output_tokens > MAX_JS_SAFE_INTEGER)
            .unwrap_or(true)
    }) {
        return Err(
            "exec pragma field `max_output_tokens` must be a non-negative safe integer".to_string(),
        );
    }

    args.code = rest.to_string();
    args.yield_time_ms = pragma.yield_time_ms;
    args.max_output_tokens = pragma.max_output_tokens;
    Ok(args)
}

pub fn is_code_mode_nested_tool(tool_name: &str) -> bool {
    tool_name != crate::PUBLIC_TOOL_NAME && tool_name != crate::WAIT_TOOL_NAME
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageDetailVisibility {
    Visible,
    Hidden,
}

pub fn build_exec_tool_description(
    enabled_tools: &[ToolDefinition],
    undeclared_tools: &[UndeclaredTool],
    namespace_descriptions: &BTreeMap<String, ToolNamespaceDescription>,
    default_exec_yield_time_ms: u64,
    code_mode_only: bool,
    image_detail_visibility: ImageDetailVisibility,
    messages: Option<&CodeModeToolMessages>,
) -> String {
    let mut sections = Vec::new();
    let image_helper = match image_detail_visibility {
        ImageDetailVisibility::Visible => LEGACY_IMAGE_HELPER_DESCRIPTION,
        ImageDetailVisibility::Hidden => UNIFIED_IMAGE_HELPER_DESCRIPTION,
    };
    let description = messages
        .and_then(|messages| messages.exec.as_ref())
        .and_then(|exec| exec.description.as_deref())
        .unwrap_or(EXEC_DESCRIPTION_TEMPLATE)
        .replace(
            "{{ default_exec_yield_time_ms }}",
            &default_exec_yield_time_ms.to_string(),
        )
        .replace("{{ image_helper }}", image_helper);
    if !description.is_empty() {
        sections.push(description);
    }
    sections.push(TOOL_DISCOVERY_GUIDANCE.to_string());
    sections.push(EXECUTION_OUTPUT_GUIDANCE.to_string());
    if undeclared_tools
        .iter()
        .any(|tool| matches!(tool, UndeclaredTool::Deferred(_)))
    {
        let guidance = messages
            .and_then(|messages| messages.deferred_nested_tools_guidance.as_deref())
            .unwrap_or(DEFERRED_NESTED_TOOLS_GUIDANCE);
        if !guidance.is_empty() {
            sections.push(guidance.to_string());
        }
    }
    sections.extend(crate::roster::render_roster(undeclared_tools));
    if !code_mode_only {
        return sections.join("\n\n");
    }

    let has_mcp_tools = enabled_tools
        .iter()
        .chain(undeclared_tools.iter().map(UndeclaredTool::definition))
        .any(|tool| mcp_structured_content_schema(tool.output_schema.as_ref()).is_some());
    if has_mcp_tools {
        let preamble = messages
            .and_then(|messages| messages.mcp_typescript_preamble.as_deref())
            .unwrap_or(MCP_TYPESCRIPT_PREAMBLE);
        if !preamble.is_empty() {
            sections.push(format!("Shared MCP Types:\n```ts\n{preamble}\n```"));
        }
    }

    if !enabled_tools.is_empty() {
        let mut current_namespace: Option<&str> = None;
        let mut nested_tool_sections = Vec::with_capacity(enabled_tools.len());

        for tool in enabled_tools {
            let name = tool.name.as_str();
            let nested_description = render_code_mode_sample_for_definition(tool);
            let namespace_description = tool
                .tool_name
                .namespace
                .as_ref()
                .and_then(|namespace| namespace_descriptions.get(namespace));
            let next_namespace = namespace_description
                .map(|namespace_description| namespace_description.name.as_str());
            if next_namespace != current_namespace {
                if let Some(namespace_description) = namespace_description {
                    let namespace_description_text = namespace_description.description.trim();
                    if !namespace_description_text.is_empty() {
                        nested_tool_sections.push(format!(
                            "## {}\n{namespace_description_text}",
                            namespace_description.name
                        ));
                    }
                }
                current_namespace = next_namespace;
            }

            let global_name = normalize_code_mode_identifier(name);
            let nested_description = nested_description.trim();
            if nested_description.is_empty() {
                nested_tool_sections.push(render_tool_heading(&global_name, name));
            } else {
                nested_tool_sections.push(format!(
                    "{}\n{nested_description}",
                    render_tool_heading(&global_name, name)
                ));
            }
        }

        sections.push(nested_tool_sections.join("\n\n"));
    }

    sections.join("\n\n")
}

pub fn build_wait_tool_description() -> &'static str {
    WAIT_DESCRIPTION_TEMPLATE
}

pub fn normalize_code_mode_identifier(tool_key: &str) -> String {
    let mut identifier = String::new();

    for (index, ch) in tool_key.chars().enumerate() {
        let is_valid = if index == 0 {
            ch == '_' || ch == '$' || ch.is_ascii_alphabetic()
        } else {
            ch == '_' || ch == '$' || ch.is_ascii_alphanumeric()
        };

        if is_valid {
            identifier.push(ch);
        } else {
            identifier.push('_');
        }
    }

    if identifier.is_empty() {
        "_".to_string()
    } else {
        identifier
    }
}

pub fn augment_tool_definition(mut definition: ToolDefinition) -> ToolDefinition {
    if definition.name != PUBLIC_TOOL_NAME {
        definition.summary = compact_tool_summary(&definition.description);
        definition.description = render_code_mode_sample_for_definition(&definition);
    }
    definition
}

pub fn render_code_mode_sample(
    description: &str,
    tool_name: &str,
    input_name: &str,
    input_type: String,
    output_type: String,
) -> String {
    let declaration = format!(
        "declare const tools: {{ {} }};",
        render_code_mode_tool_declaration(tool_name, input_name, &input_type, &output_type)
    );
    format!("{description}\n\nexec tool declaration:\n```ts\n{declaration}\n```")
}

fn render_code_mode_sample_for_definition(definition: &ToolDefinition) -> String {
    let mut names = BTreeSet::new();
    let input_name = match definition.kind {
        CodeModeToolKind::Function => "args",
        CodeModeToolKind::Freeform => "input",
    };
    let input_type = match definition.kind {
        CodeModeToolKind::Function => definition
            .input_schema
            .as_ref()
            .map(|schema| {
                render_json_schema_to_typescript_with_budget(
                    schema,
                    definition
                        .input_schema_max_bytes
                        .unwrap_or(DEFAULT_INPUT_SCHEMA_MAX_BYTES),
                    &mut names,
                )
            })
            .unwrap_or_else(|| RenderedJsonSchema::inline("unknown")),
        CodeModeToolKind::Freeform => RenderedJsonSchema::inline("string"),
    };
    let output_type = if mcp_structured_content_schema(definition.output_schema.as_ref()).is_some()
    {
        RenderedJsonSchema::inline("CallToolResult")
    } else {
        definition
            .output_schema
            .as_ref()
            .map(|schema| render_output_type(schema, COMPACT_OUTPUT_SCHEMA_MAX_BYTES, &mut names))
            .unwrap_or_else(|| RenderedJsonSchema::inline("unknown"))
    };
    let declaration = render_code_mode_tool_declaration(
        &definition.name,
        input_name,
        &input_type.root_type,
        &output_type.root_type,
    );
    format!(
        "{}\n\nexec tool declaration:\n```ts\n{}{}declare const tools: {{ {declaration} }};\n```",
        definition.description, input_type.declarations, output_type.declarations
    )
}

fn render_code_mode_tool_declaration(
    tool_name: &str,
    input_name: &str,
    input_type: &str,
    output_type: &str,
) -> String {
    let tool_name = normalize_code_mode_identifier(tool_name);
    format!("{tool_name}({input_name}: {input_type}): Promise<{output_type}>;")
}

fn render_tool_heading(global_name: &str, raw_name: &str) -> String {
    if global_name == raw_name {
        format!("### `{global_name}`")
    } else {
        format!("### `{global_name}` (`{raw_name}`)")
    }
}

#[cfg(test)]
#[path = "description_override_tests.rs"]
mod description_override_tests;

#[cfg(test)]
mod tests {
    use super::CodeModeToolKind;
    use super::ImageDetailVisibility;
    use super::ParsedExecSource;
    use super::ToolDefinition;
    use super::ToolNamespaceDescription;
    use super::UndeclaredTool;
    use super::augment_tool_definition;
    use super::build_exec_tool_description;
    use super::normalize_code_mode_identifier;
    use super::parse_exec_source;
    use crate::enabled_tool_metadata;
    use codex_protocol::ToolName;
    use pretty_assertions::assert_eq;
    use serde_json::Value as JsonValue;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn mcp_call_tool_result_schema(structured_content_schema: JsonValue) -> JsonValue {
        json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "array",
                    "items": {
                        "type": "object"
                    }
                },
                "structuredContent": structured_content_schema,
                "isError": { "type": "boolean" },
                "_meta": { "type": "object" }
            },
            "required": ["content"],
            "additionalProperties": false
        })
    }

    #[test]
    fn parse_exec_source_without_pragma() {
        assert_eq!(
            parse_exec_source("text('hi')").unwrap(),
            ParsedExecSource {
                code: "text('hi')".to_string(),
                yield_time_ms: None,
                max_output_tokens: None,
            }
        );
    }

    #[test]
    fn parse_exec_source_with_pragma() {
        assert_eq!(
            parse_exec_source("// @exec: {\"yield_time_ms\": 10}\ntext('hi')").unwrap(),
            ParsedExecSource {
                code: "text('hi')".to_string(),
                yield_time_ms: Some(10),
                max_output_tokens: None,
            }
        );
    }

    #[test]
    fn normalize_identifier_rewrites_invalid_characters() {
        assert_eq!(
            "mcp__ologs__get_profile",
            normalize_code_mode_identifier("mcp__ologs__get_profile")
        );
        assert_eq!(
            "hidden_dynamic_tool",
            normalize_code_mode_identifier("hidden-dynamic-tool")
        );
    }

    #[test]
    fn augment_tool_definition_appends_typed_declaration() {
        let definition = ToolDefinition {
            summary: String::new(),
            name: "hidden_dynamic_tool".to_string(),
            tool_name: ToolName::plain("hidden_dynamic_tool"),
            description: "Test tool".to_string(),
            kind: CodeModeToolKind::Function,
            input_schema: Some(json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
                "additionalProperties": false
            })),
            input_schema_max_bytes: None,
            output_schema: Some(json!({
                "type": "object",
                "properties": { "ok": { "type": "boolean" } },
                "required": ["ok"]
            })),
        };

        let description = augment_tool_definition(definition.clone()).description;
        assert!(description.contains("declare const tools"));
        assert!(
            description.contains(
                "hidden_dynamic_tool(args: { city: string; }): Promise<{ ok: boolean; }>;"
            )
        );
        // A large return type must not consume the input contract's independent budget.
        let mut large_output = definition;
        large_output.output_schema.as_mut().unwrap()["properties"]["ok"]["description"] =
            json!("Output detail. ".repeat(100));
        let large_output = augment_tool_definition(large_output);
        assert_eq!(
            large_output.description,
            "Test tool\n\nexec tool declaration:\n```ts\ndeclare const tools: { hidden_dynamic_tool(args: { city: string; }): Promise<unknown>; };\n```"
        );
        assert!(
            enabled_tool_metadata(&large_output)
                .describe_output()
                .contains("ok: boolean;")
        );
    }

    #[test]
    fn augment_tool_definition_includes_property_descriptions_as_comments() {
        let definition = ToolDefinition {
            summary: String::new(),
            name: "weather_tool".to_string(),
            tool_name: ToolName::plain("weather_tool"),
            description: "Weather tool".to_string(),
            kind: CodeModeToolKind::Function,
            input_schema: Some(json!({
                "type": "object",
                "properties": {
                    "weather": {
                        "type": "array",
                        "description": "look up weather for a given list of locations",
                        "items": {
                            "type": "object",
                            "properties": {
                                "location": { "type": "string" }
                            },
                            "required": ["location"]
                        }
                    }
                },
                "required": ["weather"]
            })),
            input_schema_max_bytes: None,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "forecast": {
                        "type": "string",
                        "description": "human readable weather forecast"
                    }
                },
                "required": ["forecast"]
            })),
        };

        let description = augment_tool_definition(definition).description;
        assert!(description.contains(
            r#"weather_tool(args: {
  // look up weather for a given list of locations
  weather: Array<{ location: string; }>;
}): Promise<{
  // human readable weather forecast
  forecast: string;
}>;"#
        ));
    }

    #[test]
    fn code_mode_types_structured_content_result_refs() {
        let definition = ToolDefinition {
            summary: String::new(),
            name: "mcp__sample__search".to_string(),
            tool_name: ToolName::namespaced("mcp__sample__", "search"),
            description: "Search".to_string(),
            kind: CodeModeToolKind::Function,
            input_schema: Some(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            input_schema_max_bytes: None,
            output_schema: Some(mcp_call_tool_result_schema(json!({
                "type": "object",
                "properties": {
                    "results": {
                        "type": "array",
                        "items": { "$ref": "#/definitions/Result~1item~0v1" }
                    }
                },
                "required": ["results"],
                "additionalProperties": false,
                "definitions": {
                    "Result/item~v1": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "score": { "type": "number" }
                        },
                        "required": ["id", "score"],
                        "additionalProperties": false
                    }
                }
            }))),
        };

        let schema = definition.output_schema.clone();
        let definition = augment_tool_definition(definition);
        assert_eq!(definition.output_schema, schema);
        assert_eq!(
            definition.description,
            "Search\n\nexec tool declaration:\n```ts\ndeclare const tools: { mcp__sample__search(args: {}): Promise<CallToolResult>; };\n```"
        );
        assert_eq!(
            enabled_tool_metadata(&definition).describe_output(),
            "mcp__sample__search output:\n```ts\ntype T_Result_item_v1 = { id: string; score: number; };\ntype ToolOutput = CallToolResult<{ results: Array<T_Result_item_v1>; }>;\n```"
        );
    }

    #[test]
    fn code_mode_types_shared_alias_scope_and_required_branches() {
        // Aliases share a scope across inputs and outputs, but schema refs do not.
        let definition = ToolDefinition {
            name: "send_message".to_string(),
            tool_name: ToolName::plain("send_message"),
            summary: String::new(),
            description: "Send".to_string(),
            kind: CodeModeToolKind::Function,
            input_schema: Some(json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "threadId": { "$ref": "#/$defs/Target~1Node" },
                    "threadName": { "$ref": "#/$defs/Target_Node" }
                },
                "required": ["prompt"],
                "additionalProperties": false,
                "oneOf": [{ "required": ["threadId"] }, { "required": ["threadName"] }],
                "$defs": {
                    "Target/Node": { "type": "string" },
                    "Target_Node": { "type": ["string", "null"] }
                }
            })),
            input_schema_max_bytes: None,
            output_schema: Some(json!({
                "$ref": "#/$defs/Target_Node",
                "$defs": { "Target_Node": {
                    "type": "object", "properties": {
                        "next": { "$ref": "#/$defs/Target_Node" }
                    }, "required": ["next"], "additionalProperties": false
                } }
            })),
        };
        assert_eq!(
            augment_tool_definition(definition).description,
            "Send\n\nexec tool declaration:\n```ts\ntype T_Target_Node = string;\ntype T_Target_Node_2 = string | null;\ntype T_Target_Node_3 = { next: T_Target_Node_3; };\ndeclare const tools: { send_message(args: { prompt: string; threadId?: T_Target_Node; threadName?: T_Target_Node_2; } & ({ threadId: unknown; [key: string]: unknown; } | { threadName: unknown; [key: string]: unknown; })): Promise<T_Target_Node_3>; };\n```"
        );
    }

    #[test]
    fn code_mode_only_description_includes_nested_tools() {
        let description = build_exec_tool_description(
            &[ToolDefinition {
                summary: String::new(),
                name: "foo".to_string(),
                tool_name: ToolName::plain("foo"),
                description: "bar".to_string(),
                kind: CodeModeToolKind::Function,
                input_schema: None,
                input_schema_max_bytes: None,
                output_schema: None,
            }],
            &[],
            &BTreeMap::new(),
            crate::DEFAULT_EXEC_YIELD_TIME_MS,
            /*code_mode_only*/ true,
            ImageDetailVisibility::Visible,
            /*messages*/ None,
        );
        assert!(description.contains(
            "### `foo`
bar"
        ));
        assert!(!description.contains("do not attempt to use any other tools directly"));
    }

    #[test]
    fn exec_description_mentions_timeout_helpers() {
        let description = build_exec_tool_description(
            &[],
            &[],
            &BTreeMap::new(),
            crate::DEFAULT_EXEC_YIELD_TIME_MS,
            /*code_mode_only*/ false,
            ImageDetailVisibility::Visible,
            /*messages*/ None,
        );
        assert!(description.contains("`audio(audioUrlOrItem:"));
        assert!(description.contains("`setTimeout(callback: () => void, delayMs?: number)`"));
        assert!(description.contains("`clearTimeout(timeoutId?: number)`"));
    }

    #[test]
    fn code_mode_only_description_groups_namespace_instructions_once() {
        let namespace_descriptions = BTreeMap::from([(
            "mcp__sample__".to_string(),
            ToolNamespaceDescription {
                name: "mcp__sample".to_string(),
                description: "Shared namespace guidance.".to_string(),
            },
        )]);
        let description = build_exec_tool_description(
            &[
                ToolDefinition {
                    summary: String::new(),
                    name: "mcp__sample__alpha".to_string(),
                    tool_name: ToolName::namespaced("mcp__sample__", "alpha"),
                    description: "First tool".to_string(),
                    kind: CodeModeToolKind::Function,
                    input_schema: Some(json!({
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    })),
                    input_schema_max_bytes: None,
                    output_schema: Some(mcp_call_tool_result_schema(json!({
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    }))),
                },
                ToolDefinition {
                    summary: String::new(),
                    name: "mcp__sample__beta".to_string(),
                    tool_name: ToolName::namespaced("mcp__sample__", "beta"),
                    description: "Second tool".to_string(),
                    kind: CodeModeToolKind::Function,
                    input_schema: Some(json!({
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    })),
                    input_schema_max_bytes: None,
                    output_schema: Some(mcp_call_tool_result_schema(json!({
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    }))),
                },
            ],
            &[],
            &namespace_descriptions,
            crate::DEFAULT_EXEC_YIELD_TIME_MS,
            /*code_mode_only*/ true,
            ImageDetailVisibility::Visible,
            /*messages*/ None,
        );
        assert_eq!(description.matches("## mcp__sample").count(), 1);
        assert!(description.contains("## mcp__sample\nShared namespace guidance."));
        assert!(description.contains(
            "declare const tools: { mcp__sample__alpha(args: {}): Promise<CallToolResult>; };"
        ));
        assert!(description.contains(
            "declare const tools: { mcp__sample__beta(args: {}): Promise<CallToolResult>; };"
        ));
    }

    #[test]
    fn code_mode_only_description_omits_empty_namespace_sections() {
        let namespace_descriptions = BTreeMap::from([(
            "mcp__sample__".to_string(),
            ToolNamespaceDescription {
                name: "mcp__sample".to_string(),
                description: String::new(),
            },
        )]);
        let description = build_exec_tool_description(
            &[ToolDefinition {
                summary: String::new(),
                name: "mcp__sample__alpha".to_string(),
                tool_name: ToolName::namespaced("mcp__sample__", "alpha"),
                description: "First tool".to_string(),
                kind: CodeModeToolKind::Function,
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
                input_schema_max_bytes: None,
                output_schema: Some(mcp_call_tool_result_schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }))),
            }],
            &[],
            &namespace_descriptions,
            crate::DEFAULT_EXEC_YIELD_TIME_MS,
            /*code_mode_only*/ true,
            ImageDetailVisibility::Visible,
            /*messages*/ None,
        );

        assert!(!description.contains("## mcp__sample"));
        assert!(description.contains("### `mcp__sample__alpha`"));
    }

    #[test]
    fn code_mode_only_description_renders_shared_mcp_types_once() {
        let first_tool = augment_tool_definition(ToolDefinition {
            summary: String::new(),
            name: "mcp__sample__alpha".to_string(),
            tool_name: ToolName::namespaced("mcp__sample__", "alpha"),
            description: "First tool".to_string(),
            kind: CodeModeToolKind::Function,
            input_schema: Some(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            input_schema_max_bytes: None,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "array",
                        "items": {
                            "type": "object"
                        }
                    },
                    "structuredContent": {
                        "type": "object",
                        "properties": {
                            "echo": { "type": "string" }
                        },
                        "required": ["echo"],
                        "additionalProperties": false
                    },
                    "isError": { "type": "boolean" },
                    "_meta": { "type": "object" }
                },
                "required": ["content"],
                "additionalProperties": false
            })),
        });
        let second_tool = augment_tool_definition(ToolDefinition {
            summary: String::new(),
            name: "mcp__sample__beta".to_string(),
            tool_name: ToolName::namespaced("mcp__sample__", "beta"),
            description: "Second tool".to_string(),
            kind: CodeModeToolKind::Function,
            input_schema: Some(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            input_schema_max_bytes: None,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "array",
                        "items": {
                            "type": "object"
                        }
                    },
                    "structuredContent": {
                        "type": "object",
                        "properties": {
                            "count": { "type": "integer" }
                        },
                        "required": ["count"],
                        "additionalProperties": false
                    },
                    "isError": { "type": "boolean" },
                    "_meta": { "type": "object" }
                },
                "required": ["content"],
                "additionalProperties": false
            })),
        });

        let description = build_exec_tool_description(
            &[
                ToolDefinition {
                    summary: String::new(),
                    name: first_tool.name,
                    tool_name: first_tool.tool_name,
                    description: "First tool".to_string(),
                    kind: first_tool.kind,
                    input_schema: first_tool.input_schema,
                    input_schema_max_bytes: None,
                    output_schema: first_tool.output_schema,
                },
                ToolDefinition {
                    summary: String::new(),
                    name: second_tool.name,
                    tool_name: second_tool.tool_name,
                    description: "Second tool".to_string(),
                    kind: second_tool.kind,
                    input_schema: second_tool.input_schema,
                    input_schema_max_bytes: None,
                    output_schema: second_tool.output_schema,
                },
            ],
            &[],
            &BTreeMap::new(),
            crate::DEFAULT_EXEC_YIELD_TIME_MS,
            /*code_mode_only*/ true,
            ImageDetailVisibility::Visible,
            /*messages*/ None,
        );

        assert_eq!(
            description
                .matches("type CallToolResult<TStructured = { [key: string]: unknown }>")
                .count(),
            1
        );
        assert_eq!(description.matches("Shared MCP Types:").count(), 1);
    }

    #[test]
    fn code_mode_only_description_renders_shared_mcp_types_for_deferred_tools() {
        let deferred_tool = ToolDefinition {
            summary: String::new(),
            name: "mcp__sample__alpha".to_string(),
            tool_name: ToolName::namespaced("mcp__sample__", "alpha"),
            description: "Deferred tool".to_string(),
            kind: CodeModeToolKind::Function,
            input_schema: Some(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            input_schema_max_bytes: None,
            output_schema: Some(mcp_call_tool_result_schema(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }))),
        };

        let description = build_exec_tool_description(
            &[],
            &[UndeclaredTool::Deferred(deferred_tool)],
            &BTreeMap::new(),
            crate::DEFAULT_EXEC_YIELD_TIME_MS,
            /*code_mode_only*/ true,
            ImageDetailVisibility::Visible,
            /*messages*/ None,
        );

        assert!(description.contains("Some deferred nested tools may be omitted"));
        assert!(description.contains("Shared MCP Types:"));
        assert!(!description.contains("### `mcp__sample__alpha`"));
    }

    #[test]
    fn exec_description_mentions_deferred_nested_tools_when_available() {
        let description = build_exec_tool_description(
            &[],
            &[UndeclaredTool::Deferred(ToolDefinition {
                summary: String::new(),
                name: "deferred_tool".to_string(),
                tool_name: ToolName::plain("deferred_tool"),
                description: "Deferred tool".to_string(),
                kind: CodeModeToolKind::Function,
                input_schema: None,
                input_schema_max_bytes: None,
                output_schema: None,
            })],
            &BTreeMap::new(),
            crate::DEFAULT_EXEC_YIELD_TIME_MS,
            /*code_mode_only*/ false,
            ImageDetailVisibility::Visible,
            /*messages*/ None,
        );

        assert!(description.contains("Some deferred nested tools may be omitted"));
        assert!(description.contains("search_tools(query: string"));
        assert!(description.contains("describe_tool(name: string, options?:"));
    }
}
