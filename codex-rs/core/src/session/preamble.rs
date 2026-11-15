//! Initial special context, recorded once per context window with its raw contents.
//! Counts are local o200k accounting with explicit work-limit omissions;
//! later changes are step-admission arrivals. Raw sources and model input are never cut.

use std::borrow::Cow;
use std::sync::Arc;

use codex_protocol::CensusCount;
use codex_protocol::CensusOmission;
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

pub(super) struct InjectedText {
    pub(super) role: String,
    pub(super) text: String,
}

#[derive(PartialEq)]
pub(super) enum PreambleTools<'a> {
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

/// The initial context already reported for this window, not a rolling inventory.
pub(crate) struct PreambleMark {
    window_id: codex_protocol::ContextWindowId,
    parts: Vec<PreamblePart>,
    sources: Vec<codex_protocol::ContextSource>,
}

impl PreambleMark {
    pub(super) fn restore(
        rollout: &[codex_history::RolloutItem],
        window_id: codex_protocol::ContextWindowId,
    ) -> Option<codex_protocol::ContextWindowId> {
        let mut legacy_window = true;
        for item in rollout.iter().rev() {
            match item {
                codex_history::RolloutItem::Compacted(_) => legacy_window = false,
                codex_history::RolloutItem::EventMsg(EventMsg::ItemCompleted(
                    ItemCompletedEvent {
                        item: TurnItem::Preamble(item),
                        ..
                    },
                )) if item
                    .window_id
                    .as_deref()
                    .map_or(legacy_window, |id| id == window_id.to_string()) =>
                {
                    return Some(window_id);
                }
                _ => {}
            }
        }
        None
    }
}

impl Session {
    pub(super) async fn prepare_preamble(
        &self,
        prompt: &Prompt,
    ) -> Option<AbortOnDropHandle<PreambleMark>> {
        let window_id = {
            let state = self.state.lock().await;
            let id = state.auto_compact_window_ids().window_id.clone();
            if state.preamble_window.as_ref() == Some(&id) {
                return None;
            }
            id
        };
        // The first history declaration is the window's prefix. Later declarations
        // are append-only changes, already billed as step admission arrivals.
        let tools = if prompt.tools.is_empty() {
            prompt
                .input
                .iter()
                .find_map(|item| match item {
                    ResponseItem::AdditionalTools { tools, .. } => {
                        Some(PreambleTools::History(Cow::Borrowed(tools.as_ref())))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| PreambleTools::Direct(Arc::clone(&prompt.tools)))
        } else {
            PreambleTools::Direct(Arc::clone(&prompt.tools))
        };
        let base_part = prompt.input.iter().enumerate().find_map(|(index, item)| {
            BaseInstructionsFragment::part_index(item).map(|part| (index, part))
        });
        let instructions = if prompt.base_instructions.text.is_empty() {
            base_part
                .and_then(|(index, _)| BaseInstructionsFragment::text(&prompt.input[index]))
                .unwrap_or_default()
        } else {
            &prompt.base_instructions.text
        };
        let instructions = instructions.to_string();
        let tools = tools.into_owned();
        let context: Vec<InjectedText> = prompt
            .input
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let ResponseItem::Message { role, content, .. } = item else {
                    return None;
                };
                Some((index, role, content))
            })
            .flat_map(|(index, role, content)| {
                content
                    .iter()
                    .enumerate()
                    .filter_map(move |(part, content)| {
                        if base_part == Some((index, part)) {
                            return None;
                        }
                        match content {
                            ContentItem::InputText { text }
                                if role == "developer"
                                    || role == "system"
                                    || is_contextual_user_fragment(content) =>
                            {
                                Some(InjectedText {
                                    role: role.clone(),
                                    text: text.clone(),
                                })
                            }
                            _ => None,
                        }
                    })
            })
            .collect();
        Some(AbortOnDropHandle::new(tokio::task::spawn_blocking(
            move || {
                let sources = context_sources(&instructions, &tools, &context);
                let parts = census(&instructions, &tools, context);
                PreambleMark {
                    window_id,
                    parts,
                    sources,
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
        let PreambleMark {
            window_id,
            parts,
            sources,
        } = mark;
        self.state.lock().await.preamble_window = Some(window_id.clone());
        let turn_id = &turn_context.sub_id;
        let completed_at_ms = crate::turn_timing::now_unix_timestamp_ms();
        let preamble = PreambleItem {
            id: format!("preamble:{turn_id}:{response_id}"),
            response_id: response_id.to_string(),
            window_id: Some(window_id.to_string()),
            parts,
            raw: None,
            sources,
            change: None,
            reported_input_tokens: usage.and_then(|usage| u64::try_from(usage.input_tokens).ok()),
        };
        self.send_event(
            turn_context,
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: self.thread_id,
                turn_id: turn_id.clone(),
                item: TurnItem::Preamble(preamble),
                started_at_ms: None,
                content_token_count: None,
                completed_at_ms,
            }),
        )
        .await;
    }
}

#[expect(
    clippy::expect_used,
    reason = "tool declarations contain only serializable JSON values"
)]
pub(super) fn context_sources(
    instructions: &str,
    tools: &PreambleTools<'_>,
    context: &[InjectedText],
) -> Vec<codex_protocol::ContextSource> {
    use codex_protocol::ContextSource;
    let declarations = match tools {
        PreambleTools::Direct(tools) => serde_json::to_string(tools),
        PreambleTools::History(tools) => serde_json::to_string(tools),
    }
    .expect("tool declarations are serializable");
    std::iter::once(ContextSource::Prompt {
        label: "Base instructions".into(),
        text: instructions.to_owned(),
    })
    .chain(std::iter::once(ContextSource::ToolDeclarations {
        text: declarations,
    }))
    .chain(context.iter().map(|fragment| ContextSource::Prompt {
        label: fragment.role.clone(),
        text: fragment.text.clone(),
    }))
    .collect()
}

pub(super) fn census(
    instructions: &str,
    tools: &PreambleTools<'_>,
    context: Vec<InjectedText>,
) -> Vec<PreamblePart> {
    let mut budget = CensusBudget::new();
    let mut parts = vec![PreamblePart {
        section: PreambleSection::Instructions,
        group: None,
        label: "Base instructions".to_string(),
        tokens: budget.count(instructions),
    }];
    match tools {
        PreambleTools::Direct(tools) => {
            for spec in tools.iter() {
                census_tool(spec, &mut parts, &mut budget);
            }
        }
        PreambleTools::History(tools) => {
            for spec in tools.iter() {
                census_declaration(spec, &mut parts, &mut budget);
            }
        }
    }
    for InjectedText { text, .. } in context {
        parts.push(PreamblePart {
            section: PreambleSection::Context,
            group: None,
            label: context_label(&text),
            tokens: budget.count(&text),
        });
    }
    parts
}

fn census_tool(spec: &ToolSpec, parts: &mut Vec<PreamblePart>, budget: &mut CensusBudget) {
    match spec {
        ToolSpec::Namespace(namespace) => {
            let group = Some(namespace.name.as_str());
            if !namespace.description.trim().is_empty() {
                parts.push(tool_part(
                    group,
                    "namespace description",
                    budget.count(&namespace.description),
                ));
            }
            for tool in &namespace.tools {
                let name = match tool {
                    ResponsesApiNamespaceTool::Function(tool) => &tool.name,
                    ResponsesApiNamespaceTool::Custom(tool) => &tool.name,
                };
                parts.push(tool_part(group, name, budget.serialized(tool)));
            }
        }
        ToolSpec::Freeform(tool) if tool.name == codex_code_mode::PUBLIC_TOOL_NAME => {
            census_exec(
                &tool.name,
                &tool.description,
                &tool.format.definition,
                parts,
                budget,
            );
        }
        ToolSpec::Function(_)
        | ToolSpec::Freeform(_)
        | ToolSpec::ToolSearch { .. }
        | ToolSpec::WebSearch { .. } => {
            let (group, label) = mcp_group(spec.name());
            parts.push(tool_part(group, label, budget.serialized(spec)));
        }
    }
}

fn census_declaration(spec: &Value, parts: &mut Vec<PreamblePart>, budget: &mut CensusBudget) {
    let kind = spec["type"].as_str().unwrap_or_default();
    let name = spec["name"].as_str().unwrap_or(kind);
    let description = spec["description"].as_str().unwrap_or_default();
    if kind == "namespace" {
        let group = Some(name);
        if !description.trim().is_empty() {
            parts.push(tool_part(
                group,
                "namespace description",
                budget.count(description),
            ));
        }
        if let Some(tools) = spec["tools"].as_array() {
            for tool in tools {
                parts.push(tool_part(
                    group,
                    tool["name"].as_str().unwrap_or_default(),
                    budget.serialized(tool),
                ));
            }
        }
    } else if kind == "custom" && name == codex_code_mode::PUBLIC_TOOL_NAME {
        census_exec(
            name,
            description,
            spec["format"]["definition"].as_str().unwrap_or_default(),
            parts,
            budget,
        );
    } else {
        let (group, label) = mcp_group(name);
        parts.push(tool_part(group, label, budget.serialized(spec)));
    }
}

/// Attributes the Code Mode `exec` description to the nested tools it declares.
fn census_exec(
    executor: &str,
    description: &str,
    grammar: &str,
    parts: &mut Vec<PreamblePart>,
    budget: &mut CensusBudget,
) {
    let (guidance, sections) = exec_sections(description);
    parts.push(tool_part(
        Some(executor),
        "guidance and grammar",
        budget.count(guidance) + budget.count(grammar),
    ));
    for (name, text) in sections {
        let (group, label) = mcp_group(name);
        parts.push(tool_part(
            Some(group.unwrap_or(executor)),
            label,
            budget.count(text),
        ));
    }
}

/// Splits a description rendered by `codex_code_mode::build_exec_tool_description` at its
/// nested tool headings: ``### `name` ``, optionally followed by `` (`raw name`)``.
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

fn tool_part(group: Option<&str>, label: &str, tokens: CensusCount) -> PreamblePart {
    PreamblePart {
        section: PreambleSection::Tools,
        group: group.map(str::to_string),
        label: label.to_string(),
        tokens,
    }
}

// These admission ceilings bound optional census input, not total raw-source
// capture or a wall-clock SLA. Both Preamble and /context use the same budget.
const CENSUS_BYTES: usize = 256 * 1024;
const CENSUS_CALLS: usize = 1024;

struct CensusBudget {
    bytes: usize,
    calls: usize,
}

impl CensusBudget {
    fn new() -> Self {
        Self {
            bytes: CENSUS_BYTES,
            calls: CENSUS_CALLS,
        }
    }

    fn count(&mut self, text: &str) -> CensusCount {
        if text.is_empty() {
            return CensusCount::Counted(0);
        }
        if text.len() > self.bytes || self.calls == 0 {
            return CensusCount::Omitted(CensusOmission::WorkLimit);
        }
        self.bytes -= text.len();
        self.calls -= 1;
        CensusCount::Counted(count_o200k_text(text))
    }

    #[expect(
        clippy::expect_used,
        reason = "tool declarations serialize as UTF-8 JSON; only our bounded writer may refuse"
    )]
    #[expect(
        clippy::panic,
        reason = "Tool schemas have infallible JSON representations; CensusWriter reports its only recoverable error above"
    )]
    fn serialized(&mut self, value: &impl Serialize) -> CensusCount {
        if self.bytes == 0 || self.calls == 0 {
            return CensusCount::Omitted(CensusOmission::WorkLimit);
        }
        let mut writer = CensusWriter {
            bytes: Vec::new(),
            limit: self.bytes,
        };
        match serde_json::to_writer(&mut writer, value) {
            Ok(()) => {
                self.count(std::str::from_utf8(&writer.bytes).expect("JSON serialization is UTF-8"))
            }
            Err(error) if error.io_error_kind() == Some(std::io::ErrorKind::FileTooLarge) => {
                // Serialization spent this admission. Repeated oversized tools
                // must not each get another full-sized scratch serialization.
                self.bytes = 0;
                CensusCount::Omitted(CensusOmission::WorkLimit)
            }
            Err(error) => panic!("tool declaration serialization failed: {error}"),
        }
    }
}

struct CensusWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl std::io::Write for CensusWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Injected fragments open with a tag or a heading; anything else shows its first line.
pub(super) fn context_label(text: &str) -> String {
    const WIDTH: usize = 60;
    if text.starts_with("<instruction_diff>")
        && let Some(section) = text.lines().find_map(|line| {
            line.strip_prefix("--- ")
                .and_then(|header| header.rsplit_once('/').map(|(section, _)| section))
        })
    {
        let name = if section == "agents_md" {
            "AGENTS.md".to_string()
        } else {
            section.replace('_', " ")
        };
        return format!("{name} update");
    }
    let line = text.lines().next().unwrap_or_default().trim();
    if let Some(path) = line.strip_prefix("# AGENTS.md instructions for ") {
        return format!("AGENTS.md {path}");
    }
    if let Some((tag, _)) = line.strip_prefix('<').and_then(|rest| rest.split_once('>')) {
        return tag.to_string();
    }
    match line.char_indices().nth(WIDTH) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_string(),
    }
}

#[cfg(test)]
#[path = "preamble_tests.rs"]
mod tests;
