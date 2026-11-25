//! Composes Code Mode descriptions, retaining runtime-owned tool declarations.
//! Exec templates substitute only the documented literal placeholders.

use codex_protocol::ToolName;
use codex_protocol::openai_models::CodeModeToolMessages;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::PUBLIC_TOOL_NAME;
use crate::UndeclaredTool;
use crate::discovery::compact_tool_summary;

const MAX_JS_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const MAX_YIELD_TIME_PLACEHOLDER: &str = "{{ max_yield_time_ms }}";
const MAX_OUTPUT_TOKENS_PLACEHOLDER: &str = "{{ max_output_tokens }}";
const DEFERRED_NESTED_TOOLS_GUIDANCE: &str = "`<nested_tools>` lists nested tools by name and purpose only: read a tool's contract with `describe_tool` before its first call. Later messages amend the list; `ALL_TOOLS` is always current.";
const TOOL_DISCOVERY_GUIDANCE: &str = r#"Discovery, synchronous:
- `ALL_TOOLS`: current names and purposes.
- `search_tools(query, {namespace?, limit?, offset?}?)`: tools whose name or purpose contains every word of `query`, ignoring case; an empty query lists all. `namespace` restricts the callable prefix (`functions` for plain tools). `limit` defaults to 8 and is at most 20. Returns `{matches, truncated, next_offset}`; continue with the same query and `next_offset`.
- `describe_tool(name, {detail?}?)`: the tool's instructions and parameter contract; `detail: "output"` gives the full return shape and `"schema"` the raw schemas. `name` is the exact member, optionally with a `tools.` prefix or in an unambiguous namespace-dot spelling, never a fuzzy match; call the member the contract names. Read the output shape before using result fields you have not seen.
Print bounded search results, not the whole list."#;
const EXECUTION_OUTPUT_GUIDANCE: &str = r#"Output: `text` and `notify` print an `exec_command` or `write_stdin` result as its output plus exit state, and an array of such results under ordinal headers, merging consecutive empty successes. Strings print as written; other values are serialized. Each emitted text is a section, and a result array is one section per result; `max_tokens` on an emission caps its sections together. Past the budget, sections are cut to head and tail in fair shares, and the full text of the cut sections is saved to a temporary file on the Codex host, which a remote executor may not reach. The notice names the file: read it instead of re-running. Text a nested tool truncated itself is not recoverable, so send large command output to a file and print what you need. Media do not count against the budget."#;
pub const TOOL_SEARCH_GUIDANCE: &str = r#"Use `await tools.tool_search({query: "...", limit: 8})` for ranked discovery of deferred tools. It returns `{tools: [{name, description}]}` with callable names and compact purpose summaries. Use `describe_tool(name)` for a missing contract before calling a tool."#;
const LEGACY_IMAGE_HELPER_DESCRIPTION: &str = r#"`image(value, detail?)`: append an image. Value is a base64 data URL, an object with image_url and optional detail, or an individual MCP image block (image(result.content[0])). Detail is auto, low, high, original, or null; the second argument overrides embedded detail, including MCP _meta["codex/imageDetail"]."#;
const UNIFIED_IMAGE_HELPER_DESCRIPTION: &str = r#"`image(value)`: append an image. Value is a base64 data URL, an object with image_url, or an individual MCP image block (image(result.content[0]))."#;
const AUDIO_HELPER_DESCRIPTION: &str = r#"`audio(value)`: append audio. Value is a base64 data URL, an object with audio_url, or an individual MCP audio block (audio(result.content[0]))."#;
const GENERATED_IMAGE_HELPER_DESCRIPTION: &str = r#"`generatedImage(result)`: append a generated image. Result requires image_url and may include an output_hint string. HTTP(S) URLs are unsupported."#;
const EXEC_DESCRIPTION_TEMPLATE: &str = r#"Run JavaScript as one cell: a fresh V8 async module, without Node, a console or direct network access. The input is the source alone: no JSON wrapper, no Markdown fence.
Call a nested tool with `await tools.NAME(args)`; a freeform tool takes a string. Tool permissions and approvals apply.
Await every operation: when the module finishes, its isolate is destroyed and unfinished promises are discarded.
`exec` returns when the cell finishes or calls `yield_control()`, when new input arrives, or after {{ max_yield_time_ms }} ms. A returned cell ID means the cell is still running: resume it with `wait`.
Only emitted output reaches you. A call's output budget defaults to {{ max_output_tokens }} tokens; a first line `// @exec: {"max_output_tokens": N}` sets it, within this model's limit.
- `text(value, {metadata?, max_tokens?}?)`: append one output section; `max_tokens` caps this emission. `notify(value, {metadata?}?)`: emit at once. `metadata: true` adds execution metadata.
- `yield_control()`: emit the accumulated output and return while the cell keeps running.
- `exit()`: end successfully now. `fail(message)`: end unsuccessfully now. Neither can be caught.
{{ sh_helper }}
{{ media_helpers }}
- `store(key, value)`, `load(key)`: keep serializable values under a string key across cells; a missing key loads `undefined`.
- `setTimeout(callback, delayMs?)`, `clearTimeout(id?)`: timers. A timer alone does not keep the isolate alive: await a promise to wait."#;
const EXECUTION_ORDER_GUIDANCE: &str = "`exec` and `wait` impose no order on a response's other calls: put `fence({})` between dependent stages. For a fence, a cell succeeds unless it throws, calls `fail(message)`, yields, or leaves work unawaited or in the background; tool errors it catches and nonzero exits it handles are data.";
const WAIT_DESCRIPTION_TEMPLATE: &str = "The output is only what the cell emitted since its last yield. `wait` also returns, leaving the cell running, when new user or agent input arrives or after {{ max_yield_time_ms }} ms.";
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
    pub output_schema: Option<JsonValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolNamespaceDescription {
    pub name: String,
    pub description: String,
}

/// One complete tool definition or namespace instruction and its identity in an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecDefinition {
    /// Stable identity (`Tool: tools.<name>` or `Namespace instructions: <name>`) used for diffing.
    pub key: String,
    /// Complete rendered entry, including its heading, used to replace an earlier definition.
    pub description: String,
}

/// Keys are validated before deserialization. `yield_time_ms` is accepted and ignored: calls
/// block until there is news, but upstream-trained models still send it.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
struct CodeModeExecPragma {
    #[serde(default)]
    max_output_tokens: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedExecSource {
    pub code: String,
    pub max_output_tokens: Option<usize>,
}

pub fn parse_exec_source(input: &str) -> Result<ParsedExecSource, String> {
    if input.trim().is_empty() {
        return Err(
            "exec expects raw JavaScript source text (non-empty). Provide JS only, optionally with first-line `// @exec: {\"max_output_tokens\": 1000}`.".to_string(),
        );
    }

    let mut args = ParsedExecSource {
        code: input.to_string(),
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
            "exec pragma must be a JSON object with supported field `max_output_tokens`"
                .to_string(),
        );
    }

    let value: serde_json::Value = serde_json::from_str(directive).map_err(|err| {
        format!("exec pragma must be valid JSON with supported field `max_output_tokens`: {err}")
    })?;
    let object = value.as_object().ok_or_else(|| {
        "exec pragma must be a JSON object with supported field `max_output_tokens`".to_string()
    })?;
    for key in object.keys() {
        match key.as_str() {
            "yield_time_ms" | "max_output_tokens" => {}
            _ => {
                return Err(format!(
                    "exec pragma only supports `max_output_tokens`; got `{key}`"
                ));
            }
        }
    }

    let pragma: CodeModeExecPragma = serde_json::from_value(value).map_err(|err| {
        format!("exec pragma field `max_output_tokens` must be a non-negative safe integer: {err}")
    })?;
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

/// The media helpers `exec` documents: what the model accepts and what the turn can produce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaHelpers {
    pub image_detail: ImageDetailVisibility,
    /// The model accepts audio input, so what `audio()` appends reaches it.
    pub audio: bool,
    /// The turn can generate images, so `generatedImage()` has results to forward.
    pub generated_image: bool,
}

impl MediaHelpers {
    fn image_helper(self) -> &'static str {
        match self.image_detail {
            ImageDetailVisibility::Visible => LEGACY_IMAGE_HELPER_DESCRIPTION,
            ImageDetailVisibility::Hidden => UNIFIED_IMAGE_HELPER_DESCRIPTION,
        }
    }

    /// One helper bullet per supported medium, so an unsupported one is never mentioned.
    fn render(self) -> String {
        [
            Some(self.image_helper()),
            self.audio.then_some(AUDIO_HELPER_DESCRIPTION),
            self.generated_image
                .then_some(GENERATED_IMAGE_HELPER_DESCRIPTION),
        ]
        .into_iter()
        .flatten()
        .map(|helper| format!("- {helper}"))
        .collect::<Vec<_>>()
        .join("\n")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeferredToolDiscovery {
    Catalog,
    RankedSearch,
}

pub fn build_exec_tool_description(
    enabled_tools: &[ToolDefinition],
    undeclared_tools: &[UndeclaredTool],
    namespace_descriptions: &BTreeMap<String, ToolNamespaceDescription>,
    max_yield_time_ms: u64,
    code_mode_only: bool,
    media_helpers: MediaHelpers,
    deferred_tool_discovery: DeferredToolDiscovery,
    messages: Option<&CodeModeToolMessages>,
) -> String {
    build_exec_tool_description_for(
        crate::presentation::ToolDialect::default(),
        enabled_tools,
        undeclared_tools,
        namespace_descriptions,
        max_yield_time_ms,
        code_mode_only,
        media_helpers,
        deferred_tool_discovery,
        messages,
    )
}

pub fn build_exec_tool_description_for(
    dialect: crate::presentation::ToolDialect,
    enabled_tools: &[ToolDefinition],
    undeclared_tools: &[UndeclaredTool],
    namespace_descriptions: &BTreeMap<String, ToolNamespaceDescription>,
    max_yield_time_ms: u64,
    code_mode_only: bool,
    media_helpers: MediaHelpers,
    deferred_tool_discovery: DeferredToolDiscovery,
    messages: Option<&CodeModeToolMessages>,
) -> String {
    build_exec_tool_description_and_definitions_for(
        dialect,
        enabled_tools,
        undeclared_tools,
        namespace_descriptions,
        max_yield_time_ms,
        code_mode_only,
        media_helpers,
        deferred_tool_discovery,
        false,
        messages,
    )
    .0
}

pub fn build_exec_tool_description_and_definitions(
    enabled_tools: &[ToolDefinition],
    undeclared_tools: &[UndeclaredTool],
    namespace_descriptions: &BTreeMap<String, ToolNamespaceDescription>,
    max_yield_time_ms: u64,
    code_mode_only: bool,
    media_helpers: MediaHelpers,
    deferred_tool_discovery: DeferredToolDiscovery,
    incremental_exec_enabled: bool,
    messages: Option<&CodeModeToolMessages>,
) -> (String, Vec<ExecDefinition>) {
    build_exec_tool_description_and_definitions_for(
        crate::presentation::ToolDialect::default(),
        enabled_tools,
        undeclared_tools,
        namespace_descriptions,
        max_yield_time_ms,
        code_mode_only,
        media_helpers,
        deferred_tool_discovery,
        incremental_exec_enabled,
        messages,
    )
}

pub fn build_exec_tool_description_and_definitions_for(
    dialect: crate::presentation::ToolDialect,
    enabled_tools: &[ToolDefinition],
    undeclared_tools: &[UndeclaredTool],
    namespace_descriptions: &BTreeMap<String, ToolNamespaceDescription>,
    max_yield_time_ms: u64,
    code_mode_only: bool,
    media_helpers: MediaHelpers,
    deferred_tool_discovery: DeferredToolDiscovery,
    incremental_exec_enabled: bool,
    messages: Option<&CodeModeToolMessages>,
) -> (String, Vec<ExecDefinition>) {
    let mut sections = Vec::new();
    let description = messages
        .and_then(|messages| messages.exec.as_ref())
        .and_then(|exec| exec.description.as_deref())
        .unwrap_or(EXEC_DESCRIPTION_TEMPLATE)
        .replace(MAX_YIELD_TIME_PLACEHOLDER, &max_yield_time_ms.to_string())
        .replace(
            MAX_OUTPUT_TOKENS_PLACEHOLDER,
            &crate::DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL.to_string(),
        )
        .replace(
            "{{ sh_helper }}\n",
            crate::sh::helper(
                enabled_tools
                    .iter()
                    .chain(undeclared_tools.iter().map(UndeclaredTool::definition)),
            ),
        )
        .replace("{{ media_helpers }}", &media_helpers.render())
        .replace("{{ image_helper }}", media_helpers.image_helper());
    if !description.is_empty() {
        sections.push(description);
    }
    if enabled_tools
        .iter()
        .chain(undeclared_tools.iter().map(UndeclaredTool::definition))
        .any(|tool| tool.tool_name.is_default_namespace() && tool.tool_name.name == "fs")
    {
        sections.push("- `fs.readFile(path, options?)`: a `Uint8Array`, or a UTF-8 string with `\"utf8\"` or `{encoding: \"utf8\"}`.\n- `fs.writeFile(path, data, options?)`: write a string as UTF-8, or a `Uint8Array`. Both take `{environment_id: \"...\"}`, default to the primary executor, resolve relative paths against its cwd, fail above 1 MiB, and run under your normal permissions without escalation.".into());
    }
    sections.push(EXECUTION_ORDER_GUIDANCE.to_string());
    sections.push(EXECUTION_OUTPUT_GUIDANCE.to_string());
    let default_guidance = match deferred_tool_discovery {
        // In Code Mode Only, the enabled search tool's description is rendered below.
        DeferredToolDiscovery::RankedSearch if !code_mode_only => {
            format!("{DEFERRED_NESTED_TOOLS_GUIDANCE}\n\n{TOOL_SEARCH_GUIDANCE}")
        }
        DeferredToolDiscovery::Catalog | DeferredToolDiscovery::RankedSearch => {
            DEFERRED_NESTED_TOOLS_GUIDANCE.to_string()
        }
    };
    let guidance = messages
        .and_then(|messages| messages.deferred_nested_tools_guidance.as_deref())
        .unwrap_or(&default_guidance);
    if !guidance.is_empty() {
        sections.push(guidance.to_string());
    }
    sections.push(TOOL_DISCOVERY_GUIDANCE.to_string());
    if !code_mode_only {
        return (sections.join("\n\n"), Vec::new());
    }

    // Preserve explicit caller guidance without injecting the old shared TypeScript manual.
    if let Some(preamble) =
        messages.and_then(|messages| messages.mcp_typescript_preamble.as_deref())
        && !preamble.is_empty()
    {
        sections.push(preamble.to_owned());
    }

    let mut entries = Vec::new();
    if !enabled_tools.is_empty() {
        let mut current_namespace: Option<&str> = None;
        let mut seen_namespaces = BTreeSet::new();
        let mut nested_tool_sections = Vec::with_capacity(enabled_tools.len());

        for tool in enabled_tools {
            let nested_description = render_code_mode_sample_for_definition(tool);
            let namespace_description = tool
                .tool_name
                .namespace
                .as_ref()
                .and_then(|namespace| namespace_descriptions.get(namespace));
            let next_namespace = namespace_description
                .map(|namespace_description| namespace_description.name.as_str());
            if let Some(namespace_description) = namespace_description {
                let namespace_description_text = namespace_description.description.trim();
                if !namespace_description_text.is_empty() {
                    let new_entry = incremental_exec_enabled
                        && seen_namespaces.insert(&namespace_description.name);
                    let new_group = next_namespace != current_namespace;
                    if new_entry || new_group {
                        let description = format!(
                            "## {}\n{namespace_description_text}",
                            namespace_description.name
                        );
                        if new_entry {
                            entries.push(ExecDefinition {
                                key: format!(
                                    "Namespace instructions: {}",
                                    namespace_description.name
                                ),
                                description: description.clone(),
                            });
                        }
                        if new_group {
                            nested_tool_sections.push(description);
                        }
                    }
                }
            }
            current_namespace = next_namespace;

            let heading = dialect.contract_heading(tool);
            let nested_description = nested_description.trim();
            let description = if nested_description.is_empty() {
                heading
            } else {
                format!("{heading}\n{nested_description}")
            };
            if incremental_exec_enabled {
                entries.push(ExecDefinition {
                    key: format!("Tool: tools.{}", normalize_code_mode_identifier(&tool.name)),
                    description: description.clone(),
                });
            }
            nested_tool_sections.push(description);
        }

        sections.push(nested_tool_sections.join("\n\n"));
    }

    (sections.join("\n\n"), entries)
}

pub fn build_wait_tool_description(max_yield_time_ms: u64) -> String {
    WAIT_DESCRIPTION_TEMPLATE.replace(MAX_YIELD_TIME_PLACEHOLDER, &max_yield_time_ms.to_string())
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

fn render_code_mode_sample_for_definition(definition: &ToolDefinition) -> String {
    crate::contract::render(definition)
}

#[cfg(test)]
#[path = "description_override_tests.rs"]
mod description_override_tests;

#[cfg(test)]
mod tests {
    use super::CodeModeToolKind;
    use super::DeferredToolDiscovery;
    use super::ImageDetailVisibility;
    use super::MediaHelpers;
    use super::ParsedExecSource;
    use super::ToolDefinition;
    use super::ToolNamespaceDescription;
    use super::UndeclaredTool;
    use super::augment_tool_definition;
    use super::build_exec_tool_description;
    use super::build_exec_tool_description_and_definitions;
    use super::normalize_code_mode_identifier;
    use super::parse_exec_source;
    use crate::enabled_tool_metadata;
    use codex_protocol::ToolName;
    use pretty_assertions::assert_eq;
    use serde_json::Value as JsonValue;
    use serde_json::json;
    use std::collections::BTreeMap;

    const ALL_MEDIA: MediaHelpers = MediaHelpers {
        image_detail: ImageDetailVisibility::Visible,
        audio: true,
        generated_image: true,
    };

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
                max_output_tokens: None,
            }
        );
    }

    #[test]
    fn parse_exec_source_with_pragma() {
        // Upstream-trained models still send `yield_time_ms`; rejecting it would cost a round trip.
        assert_eq!(
            parse_exec_source(
                "// @exec: {\"yield_time_ms\": 10, \"max_output_tokens\": 5}\ntext('hi')"
            )
            .unwrap(),
            ParsedExecSource {
                code: "text('hi')".to_string(),
                max_output_tokens: Some(5),
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
    fn augment_tool_definition_renders_complete_contract() {
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
            output_schema: Some(json!({
                "type": "object",
                "properties": { "ok": { "type": "boolean" } },
                "required": ["ok"]
            })),
        };

        let description = augment_tool_definition(definition.clone()).description;
        assert!(
            description.starts_with("Call: await tools.hidden_dynamic_tool(args).\nParameters:")
        );
        assert!(!description.contains("declare const tools"));
        assert!(
            description.find("city (required)").unwrap() < description.find("Returns:").unwrap()
        );
        assert!(description.ends_with("\n\nTest tool"));
        assert!(description.contains("city (required): string"));
        // A large return type must not consume the input contract's independent budget.
        let mut large_output = definition;
        large_output.output_schema.as_mut().unwrap()["properties"]["ok"]["description"] =
            json!("Output detail. ".repeat(100));
        let large_output = augment_tool_definition(large_output);
        assert!(large_output.description.contains("city (required): string"));
        assert!(
            large_output
                .description
                .contains(&"Output detail. ".repeat(100))
        );
        assert!(
            enabled_tool_metadata(large_output)
                .describe_output()
                .contains("ok (required): boolean")
        );
    }

    #[test]
    fn augment_tool_definition_retains_parameter_descriptions() {
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
        assert!(
            description.contains(
                "weather (required): array. look up weather for a given list of locations"
            )
        );
    }

    #[test]
    fn contract_resolves_structured_output_references() {
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
        assert!(
            definition
                .description
                .contains("await tools.mcp__sample__search(args)")
        );
        assert!(definition.description.contains("Returns: MCP result"));
        let output = enabled_tool_metadata(definition).describe_output();
        assert!(output.contains("id (required): string"));
        assert!(output.contains("score (required): number"));
        assert!(!output.contains("External schema"));
    }

    #[test]
    fn contract_retains_shared_references_and_required_branches() {
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
            output_schema: Some(json!({
                "$ref": "#/$defs/Target_Node",
                "$defs": { "Target_Node": {
                    "type": "object", "properties": {
                        "next": { "$ref": "#/$defs/Target_Node" }
                    }, "required": ["next"], "additionalProperties": false
                } }
            })),
        };
        let rendered = augment_tool_definition(definition).description;
        assert!(rendered.contains("Required fields: threadId"));
        assert!(rendered.contains("Required fields: threadName"));
        assert!(rendered.contains("exactly one, variant 2"));
        assert!(rendered.contains("value: string or null"));
        assert!(rendered.contains("next (required): value; see #/$defs/Target_Node"));
        assert!(!rendered.contains("External schema"));
    }

    #[test]
    fn code_mode_only_description_includes_nested_tools() {
        let (description, entries) = build_exec_tool_description_and_definitions(
            &[ToolDefinition {
                summary: String::new(),
                name: "foo".to_string(),
                tool_name: ToolName::plain("foo"),
                description: "bar".to_string(),
                kind: CodeModeToolKind::Function,
                input_schema: None,
                output_schema: None,
            }],
            &[],
            &BTreeMap::new(),
            /*max_yield_time_ms*/ 600_000,
            /*code_mode_only*/ true,
            ALL_MEDIA,
            DeferredToolDiscovery::Catalog,
            /*incremental_exec_enabled*/ false,
            /*messages*/ None,
        );
        assert!(entries.is_empty());
        assert!(description.contains(
            "### `foo`\nCall: await tools.foo(args).\nReturns: no declared shape.\n\nbar"
        ));
        assert!(!description.contains("do not attempt to use any other tools directly"));
    }

    #[test]
    fn exec_description_mentions_only_media_helpers_the_turn_supports() {
        let describe = |media_helpers| {
            build_exec_tool_description(
                &[],
                &[],
                &BTreeMap::new(),
                /*max_yield_time_ms*/ 600_000,
                /*code_mode_only*/ false,
                media_helpers,
                DeferredToolDiscovery::Catalog,
                /*messages*/ None,
            )
        };
        let mentions = |description: String| {
            ["`image(", "`audio(", "`generatedImage(", "setTimeout("]
                .map(|helper| description.contains(helper))
        };
        assert_eq!(mentions(describe(ALL_MEDIA)), [true; 4]);
        assert_eq!(
            mentions(describe(MediaHelpers {
                audio: false,
                generated_image: false,
                ..ALL_MEDIA
            })),
            [true, false, false, true]
        );
    }

    #[test]
    fn code_mode_only_definitions_match_full_description() {
        let namespace_descriptions = BTreeMap::from([(
            "mcp__sample__".to_string(),
            ToolNamespaceDescription {
                name: "mcp__sample".to_string(),
                description: "Shared namespace guidance.".to_string(),
            },
        )]);
        let enabled_tools = [
            ToolDefinition {
                summary: String::new(),
                name: "mcp__sample__alpha-tool".to_string(),
                tool_name: ToolName::namespaced("mcp__sample__", "alpha-tool"),
                description: "First tool".to_string(),
                kind: CodeModeToolKind::Function,
                input_schema: Some(json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"],
                    "additionalProperties": false
                })),
                output_schema: Some(mcp_call_tool_result_schema(json!({
                    "type": "object",
                    "properties": { "ok": { "type": "boolean" } },
                    "required": ["ok"],
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
                output_schema: Some(mcp_call_tool_result_schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }))),
            },
        ];
        let (description, entries) = build_exec_tool_description_and_definitions(
            &enabled_tools,
            &[],
            &namespace_descriptions,
            crate::DEFAULT_EXEC_YIELD_TIME_MS,
            /*code_mode_only*/ true,
            ALL_MEDIA,
            DeferredToolDiscovery::Catalog,
            /*incremental_exec_enabled*/ true,
            /*messages*/ None,
        );
        assert_eq!(description.matches("## mcp__sample").count(), 1);
        assert!(description.contains("## mcp__sample\nShared namespace guidance."));
        assert!(description.contains("Call: await tools.mcp__sample__alpha_tool(args)."));
        assert!(description.contains("Call: await tools.mcp__sample__beta(args)."));

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Namespace instructions: mcp__sample",
                "Tool: tools.mcp__sample__alpha_tool",
                "Tool: tools.mcp__sample__beta",
            ]
        );
        let catalog_start = description.find("## mcp__sample\n").unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.description.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
            &description[catalog_start..]
        );

        // Full descriptions repeat a returning namespace heading; update entries keep one identity.
        let mut plain_tool = enabled_tools[1].clone();
        plain_tool.name = "plain".to_string();
        plain_tool.tool_name = ToolName::plain("plain");
        let (description, interleaved_entries) = build_exec_tool_description_and_definitions(
            &[
                enabled_tools[0].clone(),
                plain_tool,
                enabled_tools[1].clone(),
            ],
            &[],
            &namespace_descriptions,
            /*max_yield_time_ms*/ 600_000,
            /*code_mode_only*/ true,
            ALL_MEDIA,
            DeferredToolDiscovery::Catalog,
            /*incremental_exec_enabled*/ true,
            /*messages*/ None,
        );
        assert_eq!(description.matches("## mcp__sample").count(), 2);
        assert_eq!(
            interleaved_entries
                .iter()
                .filter(|entry| entry.key.starts_with("Namespace instructions:"))
                .collect::<Vec<_>>(),
            vec![&entries[0]],
        );
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
        let (description, _) = build_exec_tool_description_and_definitions(
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
                output_schema: Some(mcp_call_tool_result_schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }))),
            }],
            &[],
            &namespace_descriptions,
            /*max_yield_time_ms*/ 600_000,
            /*code_mode_only*/ true,
            ALL_MEDIA,
            DeferredToolDiscovery::Catalog,
            /*incremental_exec_enabled*/ false,
            /*messages*/ None,
        );

        assert!(!description.lines().any(|line| line == "## mcp__sample"));
        assert!(description.contains("### `mcp__sample__alpha` (`mcp__sample__.alpha`)"));
    }

    #[test]
    fn exec_description_ignores_the_undeclared_catalog() {
        let deferred_tools = [ToolDefinition {
            summary: String::new(),
            name: "deferred_tool".to_string(),
            tool_name: ToolName::plain("deferred_tool"),
            description: "Deferred tool".to_string(),
            kind: CodeModeToolKind::Function,
            input_schema: None,
            output_schema: None,
        }];
        let search_tool = ToolDefinition {
            summary: String::new(),
            name: "tool_search".to_string(),
            tool_name: ToolName::plain("tool_search"),
            description: super::TOOL_SEARCH_GUIDANCE.to_string(),
            kind: CodeModeToolKind::Function,
            input_schema: None,
            output_schema: None,
        };
        for code_mode_only in [false, true] {
            for discovery in [
                DeferredToolDiscovery::Catalog,
                DeferredToolDiscovery::RankedSearch,
            ] {
                let enabled_tools = match discovery {
                    DeferredToolDiscovery::Catalog => Vec::new(),
                    DeferredToolDiscovery::RankedSearch => vec![search_tool.clone()],
                };
                let (description, _) = build_exec_tool_description_and_definitions(
                    &enabled_tools,
                    &deferred_tools
                        .iter()
                        .cloned()
                        .map(UndeclaredTool)
                        .collect::<Vec<_>>(),
                    &BTreeMap::new(),
                    crate::DEFAULT_EXEC_YIELD_TIME_MS,
                    code_mode_only,
                    ALL_MEDIA,
                    discovery,
                    /*incremental_exec_enabled*/ false,
                    /*messages*/ None,
                );
                assert!(description.contains(super::DEFERRED_NESTED_TOOLS_GUIDANCE));
                assert!(description.contains(super::TOOL_DISCOVERY_GUIDANCE));
                assert_eq!(
                    description.matches(super::TOOL_SEARCH_GUIDANCE).count(),
                    usize::from(discovery == DeferredToolDiscovery::RankedSearch),
                );

                let (empty_catalog_description, _) = build_exec_tool_description_and_definitions(
                    &enabled_tools,
                    &[],
                    &BTreeMap::new(),
                    crate::DEFAULT_EXEC_YIELD_TIME_MS,
                    code_mode_only,
                    ALL_MEDIA,
                    discovery,
                    /*incremental_exec_enabled*/ false,
                    /*messages*/ None,
                );
                // The roster is an instruction of its own, so a catalogue change never moves
                // the declaration.
                assert_eq!(description, empty_catalog_description);
            }
        }
    }
}
