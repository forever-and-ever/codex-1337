//! Census of the standing model context, recorded for a runtime's first request and
//! whenever its instructions or tool set change. Counts are local o200k estimates.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use codex_protocol::PreambleChange;
use codex_protocol::PreambleItem;
use codex_protocol::PreamblePart;
use codex_protocol::PreambleSection;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::TokenUsage;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolSpec;
use codex_utils_output_truncation::count_o200k_text;
use serde::Serialize;
use serde_json::Value;

use super::Session;
use super::TurnContext;
use crate::client_common::Prompt;
use crate::context::BaseInstructionsFragment;
use crate::context::is_contextual_user_fragment;
use tokio_util::task::AbortOnDropHandle;

#[derive(PartialEq)]
enum PreambleTools<'a> {
    Direct(Arc<[ToolSpec]>),
    History(Cow<'a, [Value]>),
}

impl PreambleTools<'_> {
    fn into_owned(self) -> PreambleTools<'static> {
        match self {
            Self::Direct(tools) => PreambleTools::Direct(tools),
            Self::History(tools) => PreambleTools::History(Cow::Owned(tools.into_owned())),
        }
    }
}

/// The prefix described by the runtime's latest census.
pub(crate) struct PreambleMark {
    instructions: String,
    tools: PreambleTools<'static>,
    parts: Vec<PreamblePart>,
}

impl Session {
    pub(super) async fn prepare_preamble(
        &self,
        prompt: &Prompt,
    ) -> Option<AbortOnDropHandle<PreambleMark>> {
        // The first history declaration is the window's prefix. Later declarations
        // are append-only changes, already billed as step admission arrivals.
        let tools = if prompt.tools.is_empty() {
            prompt
                .input
                .iter()
                .find_map(|item| match item {
                    ResponseItem::AdditionalTools { tools, .. } => {
                        Some(PreambleTools::History(Cow::Borrowed(tools.as_slice())))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| PreambleTools::Direct(Arc::clone(&prompt.tools)))
        } else {
            PreambleTools::Direct(Arc::clone(&prompt.tools))
        };
        let instructions = if prompt.base_instructions.text.is_empty() {
            prompt
                .input
                .iter()
                .find_map(|item| {
                    if !BaseInstructionsFragment::matches_item(item) {
                        return None;
                    }
                    let ResponseItem::Message { content, .. } = item else {
                        return None;
                    };
                    content.iter().find_map(|content| match content {
                        ContentItem::InputText { text } => Some(text.as_str()),
                        _ => None,
                    })
                })
                .unwrap_or_default()
        } else {
            &prompt.base_instructions.text
        };
        if self
            .state
            .lock()
            .await
            .preamble_mark
            .as_ref()
            .is_some_and(|mark| mark.instructions == instructions && mark.tools == tools)
        {
            return None;
        }
        let instructions = instructions.to_string();
        let tools = tools.into_owned();
        let context: Vec<String> = prompt
            .input
            .iter()
            .filter_map(|item| {
                if BaseInstructionsFragment::matches_item(item) {
                    return None;
                }
                let ResponseItem::Message { role, content, .. } = item else {
                    return None;
                };
                Some((role, content))
            })
            .flat_map(|(role, content)| {
                content.iter().filter_map(move |content| match content {
                    ContentItem::InputText { text }
                        if role == "developer" || is_contextual_user_fragment(content) =>
                    {
                        Some(text.clone())
                    }
                    _ => None,
                })
            })
            .collect();
        Some(AbortOnDropHandle::new(tokio::task::spawn_blocking(
            move || {
                let parts = census(&instructions, &tools, context);
                PreambleMark {
                    instructions,
                    tools,
                    parts,
                }
            },
        )))
    }

    /// Emits the census as its own item, never part of the response's completion record:
    /// each rollout line projects at most one item.
    pub(super) async fn record_preamble(
        &self,
        turn_context: &TurnContext,
        mark: PreambleMark,
        response_id: &str,
        usage: Option<&TokenUsage>,
    ) {
        let parts = mark.parts.clone();
        let change = {
            let mut state = self.state.lock().await;
            let change = state
                .preamble_mark
                .as_ref()
                .map(|previous| change(&previous.parts, &parts));
            state.preamble_mark = Some(mark);
            change
        };
        let turn_id = &turn_context.sub_id;
        let completed_at_ms = crate::turn_timing::now_unix_timestamp_ms();
        let preamble = PreambleItem {
            id: format!("preamble:{turn_id}:{response_id}"),
            response_id: response_id.to_string(),
            completed_at_ms,
            parts,
            change,
            reported_input_tokens: usage.and_then(|usage| u64::try_from(usage.input_tokens).ok()),
        };
        self.send_event(
            turn_context,
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: self.thread_id,
                turn_id: turn_id.clone(),
                item: TurnItem::Preamble(preamble),
                started_at_ms: None,
                completed_at_ms,
            }),
        )
        .await;
    }
}

fn census(
    instructions: &str,
    tools: &PreambleTools<'_>,
    context: Vec<String>,
) -> Vec<PreamblePart> {
    let mut parts = vec![PreamblePart {
        section: PreambleSection::Instructions,
        group: None,
        label: "Base instructions".to_string(),
        tokens: count_o200k_text(instructions),
    }];
    match tools {
        PreambleTools::Direct(tools) => {
            for spec in tools.iter() {
                census_tool(spec, &mut parts);
            }
        }
        PreambleTools::History(tools) => {
            for spec in tools.iter() {
                census_declaration(spec, &mut parts);
            }
        }
    }
    for text in context {
        parts.push(PreamblePart {
            section: PreambleSection::Context,
            group: None,
            label: context_label(&text),
            tokens: count_o200k_text(&text),
        });
    }
    parts
}

fn census_tool(spec: &ToolSpec, parts: &mut Vec<PreamblePart>) {
    match spec {
        ToolSpec::Namespace(namespace) => {
            let group = Some(namespace.name.as_str());
            if !namespace.description.trim().is_empty() {
                parts.push(tool_part(
                    group,
                    "namespace description",
                    count_o200k_text(&namespace.description),
                ));
            }
            for tool in &namespace.tools {
                let name = match tool {
                    ResponsesApiNamespaceTool::Function(tool) => &tool.name,
                    ResponsesApiNamespaceTool::Custom(tool) => &tool.name,
                };
                parts.push(tool_part(group, name, serialized_tokens(tool)));
            }
        }
        ToolSpec::Freeform(tool) if tool.name == codex_code_mode::PUBLIC_TOOL_NAME => {
            census_exec(
                &tool.name,
                &tool.description,
                &tool.format.definition,
                parts,
            );
        }
        ToolSpec::Function(_)
        | ToolSpec::Freeform(_)
        | ToolSpec::ToolSearch { .. }
        | ToolSpec::WebSearch { .. } => {
            let (group, label) = mcp_group(spec.name());
            parts.push(tool_part(group, label, serialized_tokens(spec)));
        }
    }
}

fn census_declaration(spec: &Value, parts: &mut Vec<PreamblePart>) {
    let kind = spec["type"].as_str().unwrap_or_default();
    let name = spec["name"].as_str().unwrap_or(kind);
    let description = spec["description"].as_str().unwrap_or_default();
    if kind == "namespace" {
        let group = Some(name);
        if !description.trim().is_empty() {
            parts.push(tool_part(
                group,
                "namespace description",
                count_o200k_text(description),
            ));
        }
        if let Some(tools) = spec["tools"].as_array() {
            for tool in tools {
                parts.push(tool_part(
                    group,
                    tool["name"].as_str().unwrap_or_default(),
                    serialized_tokens(tool),
                ));
            }
        }
    } else if kind == "custom" && name == codex_code_mode::PUBLIC_TOOL_NAME {
        census_exec(
            name,
            description,
            spec["format"]["definition"].as_str().unwrap_or_default(),
            parts,
        );
    } else {
        let (group, label) = mcp_group(name);
        parts.push(tool_part(group, label, serialized_tokens(spec)));
    }
}

/// Attributes the Code Mode `exec` description to the nested tools it declares.
fn census_exec(executor: &str, description: &str, grammar: &str, parts: &mut Vec<PreamblePart>) {
    let (guidance, sections) = exec_sections(description);
    parts.push(tool_part(
        Some(executor),
        "guidance and grammar",
        count_o200k_text(guidance) + count_o200k_text(grammar),
    ));
    for (name, text) in sections {
        let (group, label) = mcp_group(name);
        parts.push(tool_part(
            Some(group.unwrap_or(executor)),
            label,
            count_o200k_text(text),
        ));
    }
}

/// Splits a description rendered by `codex_code_mode::build_exec_tool_description` at its
/// nested tool headings: `### \`name\``, optionally followed by ` (\`raw name\`)`.
fn exec_sections(description: &str) -> (&str, Vec<(&str, &str)>) {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in description.split_inclusive('\n') {
        if let Some(name) = tool_heading(line.trim_end()) {
            starts.push((offset, name));
        }
        offset += line.len();
    }
    let Some(&(first, _)) = starts.first() else {
        return (description, Vec::new());
    };
    let sections = starts
        .iter()
        .enumerate()
        .map(|(index, &(start, name))| {
            let end = starts
                .get(index + 1)
                .map_or(description.len(), |&(next, _)| next);
            (name, &description[start..end])
        })
        .collect();
    (&description[..first], sections)
}

fn tool_heading(line: &str) -> Option<&str> {
    let (name, tail) = line.strip_prefix("### `")?.split_once('`')?;
    let identifier = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric());
    let tail = tail.is_empty() || (tail.starts_with(" (`") && tail.ends_with("`)"));
    (identifier && tail).then_some(name)
}

/// MCP tools are named `mcp__<server>__<tool>`; they group by server.
fn mcp_group(name: &str) -> (Option<&str>, &str) {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map_or((None, name), |(server, tool)| {
            (Some(&name[.."mcp__".len() + server.len()]), tool)
        })
}

fn tool_part(group: Option<&str>, label: &str, tokens: u64) -> PreamblePart {
    PreamblePart {
        section: PreambleSection::Tools,
        group: group.map(str::to_string),
        label: label.to_string(),
        tokens,
    }
}

fn serialized_tokens(value: &impl Serialize) -> u64 {
    serde_json::to_string(value).map_or(0, |json| count_o200k_text(&json))
}

/// Injected fragments open with a tag or a heading; anything else shows its first line.
pub(super) fn context_label(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if let Some(path) = line.strip_prefix("# AGENTS.md instructions for ") {
        return format!("AGENTS.md {path}");
    }
    if let Some((tag, _)) = line.strip_prefix('<').and_then(|rest| rest.split_once('>')) {
        return tag.to_string();
    }
    const WIDTH: usize = 60;
    match line.char_indices().nth(WIDTH) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_string(),
    }
}

fn change(previous: &[PreamblePart], parts: &[PreamblePart]) -> PreambleChange {
    fn totals(parts: &[PreamblePart]) -> BTreeMap<String, u64> {
        let mut totals = BTreeMap::new();
        for part in parts {
            let key = match &part.group {
                Some(group) => format!("{group}/{}", part.label),
                None => part.label.clone(),
            };
            *totals.entry(key).or_default() += part.tokens;
        }
        totals
    }
    let (before, after) = (totals(previous), totals(parts));
    PreambleChange {
        previous_tokens: before.values().sum(),
        added: after
            .keys()
            .filter(|key| !before.contains_key(*key))
            .cloned()
            .collect(),
        removed: before
            .keys()
            .filter(|key| !after.contains_key(*key))
            .cloned()
            .collect(),
        resized: after
            .iter()
            .filter(|(key, tokens)| before.get(*key).is_some_and(|previous| previous != *tokens))
            .map(|(key, _)| key.clone())
            .collect(),
    }
}

#[cfg(test)]
#[path = "preamble_tests.rs"]
mod tests;
