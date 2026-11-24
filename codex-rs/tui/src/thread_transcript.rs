//! Project persisted items into the same rich cells as completed live output.
//! Tool grouping spans hidden reasoning but stops at visible content and turn boundaries.
//! Shell invocations stay independent until an enclosing Code Mode return groups them.

use std::sync::Arc;

use crate::app_server_session::AppServerSession;
use crate::app_server_session::HistoryHydrationScope;
use crate::git_action_directives::parse_assistant_markdown;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::ReasoningSummaryCell;
use crate::history_cell::UserHistoryCell;
use crate::history_cell::split_reasoning_summary_parts;
use crate::inline_visualization::InlineVisualizationContext;
use crate::legacy_core::config::Config;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::UserInput;
use codex_protocol::ThreadId;
use codex_protocol::items::UserMessageItem;
use ratatui::style::Stylize as _;
use std::path::Path;

mod activity_pages;
mod computer_groups;
mod other_items;
pub(crate) mod tools;

pub(crate) use activity_pages::fold_trailing_activity_details;
pub(crate) use activity_pages::is_hidden_activity_detail;
#[allow(
    unused_imports,
    reason = "Used by later layers of the TUI refresh stack."
)]
pub(crate) use computer_groups::join_computer_groups;
pub(crate) use computer_groups::older_computer_group;

pub(crate) type TranscriptCells = Vec<Arc<dyn HistoryCell>>;

fn is_activity_metadata(item: &ThreadItemPayload) -> bool {
    matches!(
        item,
        ThreadItemPayload::Reasoning { .. }
            | ThreadItemPayload::ToolOutputTokens { .. }
            | ThreadItemPayload::StepAdmission(_)
            | ThreadItemPayload::ToolInput(_)
    ) || crate::history_cell::ToolOutputCell::is_item(item)
}

fn flush_computer(
    pending: &mut Option<crate::history_cell::ComputerActivityCell>,
    cells: &mut TranscriptCells,
) {
    if let Some(group) = pending.take() {
        push_activity_cell(cells, Arc::new(group));
    }
}

fn push_activity_cell(cells: &mut TranscriptCells, cell: Arc<dyn HistoryCell>) {
    if let Some(members) = crate::history_cell::split_code_mode_members(cells, &cell) {
        for member in members {
            push_activity_cell(cells, member);
        }
        return;
    }
    if let Some((range, replacements)) = crate::history_cell::reconcile_agent_wait(cells, &cell)
        .or_else(|| crate::history_cell::reconcile_code_mode(cells, &cell))
    {
        cells.splice(range, replacements);
    } else if crate::multi_agents::AgentToolHistoryCell::wait_for(cell.as_ref()).is_some()
        || crate::multi_agents::AgentToolHistoryCell::resume_for(cell.as_ref()).is_some()
    {
        let reconciliation = crate::history_cell::reconcile_tool_calls(cells, &cell);
        for (index, replacement) in reconciliation.replacements {
            cells[index] = replacement;
        }
        cells.extend(reconciliation.remainder);
    } else {
        cells.push(cell);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RawReasoningVisibility {
    Hidden,
    Visible,
}

pub(crate) async fn load_session_transcript(
    app_server: &mut AppServerSession,
    thread_id: ThreadId,
    raw_reasoning_visibility: RawReasoningVisibility,
    config: Option<&Config>,
    metadata: &(dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata + Sync),
    image_origin: crate::history_cell::ImagePathOrigin,
) -> std::io::Result<TranscriptCells> {
    let mut thread = app_server
        .thread_read(thread_id, /*include_turns*/ false)
        .await
        .map_err(std::io::Error::other)?;
    app_server
        .hydrate_initial_thread_history(
            &mut thread,
            /*turn_cursor*/ None,
            /*item_cursor*/ None,
            /*config*/ None,
            /*local_settings*/ None,
            HistoryHydrationScope::FullTranscript,
        )
        .await
        .map_err(std::io::Error::other)?;
    Ok(thread_to_transcript_cells(
        thread,
        raw_reasoning_visibility,
        config,
        metadata,
        image_origin,
    ))
}

pub(crate) fn thread_to_transcript_cells(
    thread: Thread,
    raw_reasoning_visibility: RawReasoningVisibility,
    config: Option<&Config>,
    metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
    image_origin: crate::history_cell::ImagePathOrigin,
) -> TranscriptCells {
    let cwd = thread.cwd;
    let thread_id = ThreadId::from_string(&thread.id).ok();
    let mut cells = Vec::new();
    let mut owned = crate::code_mode_transcript::Reducers::default();
    for turn in thread.turns {
        append_thread_items(
            &mut cells,
            &mut owned,
            thread_id,
            cwd.as_path(),
            turn.items,
            raw_reasoning_visibility,
            config,
            metadata,
            image_origin,
        );
        if let Some(error) = turn.error {
            cells.push(Arc::new(crate::history_cell::new_error_event(
                error.message,
            )));
        }
    }
    if cells.is_empty() {
        cells.push(Arc::new(PlainHistoryCell::new(vec![
            "No transcript content available".italic().dim().into(),
        ])));
    }
    cells
}

pub(crate) fn thread_items_to_transcript_cells(
    thread_id: Option<ThreadId>,
    cwd: &Path,
    items: impl IntoIterator<Item = ThreadItemPayload>,
    raw_reasoning_visibility: RawReasoningVisibility,
    config: Option<&Config>,
    metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
    image_origin: crate::history_cell::ImagePathOrigin,
) -> TranscriptCells {
    let mut cells = Vec::new();
    let mut owned = crate::code_mode_transcript::Reducers::default();
    append_thread_items(
        &mut cells,
        &mut owned,
        thread_id,
        cwd,
        items.into_iter().map(ThreadItem::untimed),
        raw_reasoning_visibility,
        config,
        metadata,
        image_origin,
    );
    cells
}

#[expect(
    clippy::too_many_arguments,
    reason = "private body shared by the two projection entry points"
)]
fn append_thread_items(
    cells: &mut TranscriptCells,
    owned: &mut crate::code_mode_transcript::Reducers,
    thread_id: Option<ThreadId>,
    cwd: &Path,
    items: impl IntoIterator<Item = ThreadItem>,
    raw_reasoning_visibility: RawReasoningVisibility,
    config: Option<&Config>,
    metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
    image_origin: crate::history_cell::ImagePathOrigin,
) {
    let inline_visualization_context = config.and_then(|config| {
        thread_id.and_then(|thread_id| InlineVisualizationContext::from_config(config, thread_id))
    });
    let mut pending: Option<crate::history_cell::ComputerActivityCell> = None;
    for ThreadItem {
        payload: item,
        started_at_ms,
        completed_at_ms,
    } in items
    {
        if let ThreadItemPayload::CodeModeOwned(scope) = item {
            flush_computer(&mut pending, cells);
            let display = thread_id.unwrap_or(scope.owner.thread_id);
            match owned.admit(
                display,
                crate::code_mode_transcript::Admission::item(scope, String::new()),
                cwd,
                /*animations*/ false,
                metadata,
                image_origin,
            ) {
                Ok(projection) => {
                    let incoming = projection
                        .block
                        .as_any()
                        .downcast_ref::<crate::history_cell::CodeModeCell>();
                    if let Some(index) = cells.iter().position(|cell| {
                        cell.as_any()
                            .downcast_ref::<crate::history_cell::CodeModeCell>()
                            .is_some_and(|block| {
                                incoming.is_some_and(|incoming| {
                                    block.identity() == incoming.identity()
                                        && block.owner().is_some()
                                })
                            })
                    }) {
                        cells[index] = projection.block;
                    } else {
                        cells.push(projection.block);
                    }
                    if let Some(wait) = projection.wait {
                        if let Some(index) = cells
                            .iter()
                            .position(|cell| cell.activity_ids() == wait.activity_ids())
                        {
                            if !cells[index]
                                .as_any()
                                .downcast_ref::<crate::history_cell::CodeModeWaitCell>()
                                .is_some_and(crate::history_cell::CodeModeWaitCell::completed)
                            {
                                cells[index] = wait;
                            }
                        } else {
                            cells.push(wait);
                        }
                    }
                }
                Err(error) => tracing::warn!(%error, "rejecting conflicting owned history item"),
            }
            continue;
        }
        if matches!(item, ThreadItemPayload::Reasoning { .. })
            && let Some(group) = &mut pending
            && !cells
                .iter()
                .filter_map(|cell| {
                    cell.as_any()
                        .downcast_ref::<crate::history_cell::CodeModeCell>()
                })
                .any(|block| block.owns(group))
        {
            for cell in item_to_cells(
                ThreadItem {
                    payload: item,
                    started_at_ms,
                    completed_at_ms,
                },
                cwd,
                raw_reasoning_visibility,
                inline_visualization_context.clone(),
                metadata,
                image_origin,
            ) {
                group.group.push_detail(cell);
            }
            continue;
        }
        if let Some(cell) = tools::historical_tool_fallback(&item) {
            flush_computer(&mut pending, cells);
            push_activity_cell(
                cells,
                Arc::new(crate::history_cell::ToolItemCell::new(
                    item.id().into(),
                    cell,
                )),
            );
            continue;
        }
        match item {
            item @ ThreadItemPayload::McpToolCall { .. } => {
                let Some(call) = tools::McpHistory::from_item(item) else {
                    flush_computer(&mut pending, cells);
                    continue;
                };
                if call.invocation.is_computer_activity() {
                    computer_groups::append(pending.get_or_insert_default(), call);
                } else {
                    flush_computer(&mut pending, cells);
                    let cell = call.into_cell();
                    push_activity_cell(cells, Arc::new(cell));
                }
            }
            item @ ThreadItemPayload::CommandExecution { .. } => {
                flush_computer(&mut pending, cells);
                if let Some(command) = tools::CommandHistory::from_item(item) {
                    push_activity_cell(cells, Arc::new(command.into_cell()));
                }
            }
            item => {
                let projected = item_to_cells(
                    ThreadItem {
                        payload: item,
                        started_at_ms,
                        completed_at_ms,
                    },
                    cwd,
                    raw_reasoning_visibility,
                    inline_visualization_context.clone(),
                    metadata,
                    image_origin,
                );
                if !projected.is_empty() {
                    flush_computer(&mut pending, cells);
                    for cell in projected {
                        if let Some(thought) = cell
                            .as_any()
                            .downcast_ref::<crate::history_cell::ThoughtCell>()
                            && cells.iter().any(|cell| {
                                cell.as_any()
                                    .downcast_ref::<crate::history_cell::ThoughtCell>()
                                    .is_some_and(|existing| existing.id == thought.id)
                            })
                        {
                            continue;
                        }
                        if let Some((range, replacements)) =
                            crate::history_cell::reconcile_agent_wait(cells, &cell)
                                .or_else(|| crate::history_cell::reconcile_code_mode(cells, &cell))
                                .or_else(|| {
                                    crate::history_cell::ToolOutputGroup::attach(
                                        cells,
                                        Arc::clone(&cell),
                                    )
                                })
                        {
                            cells.splice(range, replacements);
                        } else {
                            push_activity_cell(cells, cell);
                        }
                    }
                }
            }
        }
    }
    flush_computer(&mut pending, cells);
    crate::prompt_order::normalize(cells);
}

/// Project one item without changing the active widget or its turn lifecycle.
fn item_to_cells(
    item: ThreadItem,
    cwd: &Path,
    raw_reasoning_visibility: RawReasoningVisibility,
    inline_visualization_context: Option<InlineVisualizationContext>,
    metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
    image_origin: crate::history_cell::ImagePathOrigin,
) -> TranscriptCells {
    let item_started_at_ms = item.started_at_ms;
    let item_completed_at_ms = item.completed_at_ms;
    let mut cells: TranscriptCells = Vec::new();
    match item.payload {
        item @ ThreadItemPayload::Thought { .. } => {
            cells.push(Arc::new(crate::history_cell::ThoughtCell::from_item(
                ThreadItem {
                    payload: item,
                    started_at_ms: item_started_at_ms,
                    completed_at_ms: item_completed_at_ms,
                },
                cwd,
            )))
        }
        item @ ThreadItemPayload::Preamble { .. } => {
            cells.push(Arc::new(crate::history_cell::PreambleCell::from_item(
                ThreadItem {
                    payload: item,
                    started_at_ms: item_started_at_ms,
                    completed_at_ms: item_completed_at_ms,
                },
            )));
        }
        ThreadItemPayload::Arrival(item) => {
            cells.push(Arc::new(crate::history_cell::ArrivalCell::new(item)));
        }
        ThreadItemPayload::UserMessage {
            id,
            client_id,
            source_response_item_id,
            content,
        } => {
            if content.iter().any(|input| {
                matches!(
                    input,
                    UserInput::Audio { .. } | UserInput::LocalAudio { .. }
                )
            }) {
                tracing::warn!(
                    user_message_id = id,
                    "audio user inputs are not supported by the TUI and will be omitted"
                );
            }
            let item = UserMessageItem {
                id,
                client_id: client_id.clone(),
                source_response_item_id: source_response_item_id.clone(),
                content: content
                    .into_iter()
                    .map(codex_app_server_protocol::UserInput::into_core)
                    .collect(),
            };
            let message = item.message();
            let replies = crate::async_question_reply::parse(&message).unwrap_or_default();
            let (message, elements) = if replies.is_empty() {
                (message, item.text_elements())
            } else {
                (
                    crate::async_question_reply::display_replies(&replies),
                    Vec::new(),
                )
            };
            cells.push(Arc::new(
                UserHistoryCell {
                    timestamp: None,
                    presentation: Default::default(),
                    prompt_identity: crate::prompt_order::UserPromptIdentity {
                        client_id,
                        source_response_item_id,
                    },
                    message,
                    text_elements: elements,
                    local_image_paths: item.local_image_paths(),
                    remote_image_urls: item.image_urls(),
                }
                .with_async_replies(replies, cwd),
            ));
        }
        ThreadItemPayload::AgentMessage {
            id,
            text,
            questions,
            ..
        } => {
            let parsed = parse_assistant_markdown(&text, cwd);
            if !parsed.visible_markdown.trim().is_empty() {
                let mut cell = AgentMarkdownCell::new_with_inline_visualizations(
                    parsed.visible_markdown,
                    cwd,
                    inline_visualization_context,
                );
                cell = cell.with_questions(questions.as_deref().unwrap_or_default());
                cell.item_id = Some(id);
                cells.push(Arc::new(cell));
            }
        }
        ThreadItemPayload::FunctionCallOutput {
            id,
            call_id,
            name,
            namespace,
            output,
        } => {
            if let Some(cell) = crate::dynamic_tools::unsolicited_tool_output(
                call_id.as_deref(),
                &name,
                namespace.as_deref(),
                &output,
            ) {
                cells.push(Arc::from(cell));
            } else if let Some(cell) = crate::history_cell::ToolOutputCell::from_output(
                &id,
                call_id.as_deref(),
                &name,
                namespace.as_deref(),
                &output,
            ) {
                cells.push(Arc::new(cell));
            }
        }
        ThreadItemPayload::Plan { text, .. } => {
            if !text.trim().is_empty() {
                cells.push(Arc::new(crate::history_cell::new_proposed_plan(text, cwd)));
            }
        }
        ThreadItemPayload::Reasoning {
            id,
            summary,
            content,
        } => {
            let (header, mut text) = split_reasoning_summary_parts(&summary);
            if matches!(raw_reasoning_visibility, RawReasoningVisibility::Visible)
                && !content.is_empty()
            {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&content.join("\n\n"));
            }
            if !text.trim().is_empty() {
                let mut cell =
                    ReasoningSummaryCell::new(header, text, cwd, /*transcript_only*/ true);
                cell.set_source_item_id(id);
                cells.push(Arc::new(cell));
            }
        }
        item @ ThreadItemPayload::CommandExecution { .. } => {
            if let Some(command) = tools::CommandHistory::from_item(item) {
                cells.push(Arc::new(command.into_cell()));
            }
        }
        other => cells.extend(other_items::cells(other, cwd, metadata, image_origin)),
    }
    cells
}

/// Project a scoped child without entering the widget's ordinary active-cell lifecycle.
pub(crate) fn scoped_item_cells(
    item: ThreadItemPayload,
    cwd: &Path,
    source: crate::code_mode_transcript::Source,
    metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
    image_origin: crate::history_cell::ImagePathOrigin,
) -> TranscriptCells {
    if let ThreadItemPayload::ToolInput(input) = item {
        return vec![Arc::new(crate::history_cell::ToolInputCell(input))];
    }
    if let ThreadItemPayload::CollabAgentToolCall { .. } = &item {
        return crate::multi_agents::tool_call_history_cell(
            &item, /*cached_spawn_request*/ None, metadata,
        )
        .into_iter()
        .map(|cell| Arc::new(cell) as Arc<dyn HistoryCell>)
        .collect();
    }
    if source == crate::code_mode_transcript::Source::Live
        && let ThreadItemPayload::FileChange {
            id,
            changes,
            status: codex_app_server_protocol::PatchApplyStatus::InProgress,
        } = &item
        && !changes.is_empty()
    {
        return crate::history_cell::new_patch_events(
            crate::app_server_approval_conversions::file_update_changes_to_display(changes.clone()),
            cwd,
        )
        .into_iter()
        .map(|cell| Arc::new(cell.with_activity_id(id.clone())) as Arc<dyn HistoryCell>)
        .collect();
    }
    // A frozen MCP invocation still has exact semantic arguments. Do not flatten
    // historical page prefixes into an opaque status string before formatting.
    if (source == crate::code_mode_transcript::Source::Live
        || matches!(&item, ThreadItemPayload::McpToolCall { .. }))
        && let Some(cell) = tools::pending_tool_cell(&item)
    {
        return vec![cell];
    }
    if let Some(cell) = tools::historical_tool_fallback(&item) {
        return vec![Arc::new(crate::history_cell::ToolItemCell::new(
            item.id().into(),
            cell,
        ))];
    }
    if let Some(call) = tools::McpHistory::from_item(item.clone()) {
        return vec![Arc::new(call.into_cell())];
    }
    item_to_cells(
        ThreadItem::untimed(item),
        cwd,
        RawReasoningVisibility::Hidden,
        /*inline_visualization_context*/ None,
        metadata,
        image_origin,
    )
}
