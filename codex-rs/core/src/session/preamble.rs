//! Census of the standing model context, recorded for a runtime's first request and
//! whenever its instructions or tool set change. Counts are local o200k estimates.

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
use codex_tools::FreeformTool;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolSpec;
use codex_utils_output_truncation::count_o200k_text;
use serde::Serialize;

use super::Session;
use super::TurnContext;
use crate::client_common::Prompt;
use crate::context::is_contextual_user_fragment;

/// The prefix described by the runtime's latest census.
pub(crate) struct PreambleMark {
    instructions: String,
    tools: Arc<[ToolSpec]>,
    parts: Vec<PreamblePart>,
}

impl Session {
    /// Completes the Preamble of `response_id` when the instructions or tools its `prompt`
    /// carried differ from the latest census. The Preamble is an item of its own, never part
    /// of the response's completion record: each rollout line projects at most one item.
    pub(super) async fn record_preamble(
        &self,
        turn_context: &TurnContext,
        prompt: &Prompt,
        response_id: &str,
        usage: Option<&TokenUsage>,
    ) {
        let previous = match &self.state.lock().await.preamble_mark {
            Some(mark)
                if mark.instructions == prompt.base_instructions.text
                    && (Arc::ptr_eq(&mark.tools, &prompt.tools) || mark.tools == prompt.tools) =>
            {
                return;
            }
            mark => mark.as_ref().map(|mark| mark.parts.clone()),
        };
        let parts = census(prompt);
        let change = previous.map(|previous| change(&previous, &parts));
        self.state.lock().await.preamble_mark = Some(PreambleMark {
            instructions: prompt.base_instructions.text.clone(),
            tools: Arc::clone(&prompt.tools),
            parts: parts.clone(),
        });
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

fn census(prompt: &Prompt) -> Vec<PreamblePart> {
    let mut parts = vec![PreamblePart {
        section: PreambleSection::Instructions,
        group: None,
        label: "Base instructions".to_string(),
        tokens: count_o200k_text(&prompt.base_instructions.text),
    }];
    for spec in prompt.tools.iter() {
        census_tool(spec, &mut parts);
    }
    for item in &prompt.input {
        let ResponseItem::Message { role, content, .. } = item else {
            continue;
        };
        for content in content {
            if let ContentItem::InputText { text } = content
                && (role == "developer" || is_contextual_user_fragment(content))
            {
                parts.push(PreamblePart {
                    section: PreambleSection::Context,
                    group: None,
                    label: context_label(text),
                    tokens: count_o200k_text(text),
                });
            }
        }
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
            census_exec(tool, parts);
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

/// Attributes the Code Mode `exec` description to the nested tools it declares.
fn census_exec(tool: &FreeformTool, parts: &mut Vec<PreamblePart>) {
    let (guidance, sections) = exec_sections(&tool.description);
    parts.push(tool_part(
        Some(&tool.name),
        "guidance and grammar",
        count_o200k_text(guidance) + count_o200k_text(&tool.format.definition),
    ));
    for (name, text) in sections {
        let (group, label) = mcp_group(name);
        parts.push(tool_part(
            Some(group.unwrap_or(&tool.name)),
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
fn context_label(text: &str) -> String {
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
