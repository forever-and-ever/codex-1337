//! Restore historical tool and notice cells without changing live application state.
//!
//! Formatting stays in the same history cells used by live events. Only completed
//! patches render as applied changes; other statuses retain their actual outcome.

use super::TranscriptCells;
use crate::app_server_approval_conversions::file_update_changes_to_display;
use crate::history_cell;
use crate::history_cell::PlainHistoryCell;
use crate::multi_agents;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::WebSearchAction;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use ratatui::style::Stylize as _;
use std::sync::Arc;

pub(super) fn cells(
    item: ThreadItemPayload,
    cwd: &AbsolutePathBuf,
    metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
    image_origin: crate::history_cell::ImagePathOrigin,
) -> TranscriptCells {
    let call_id = matches!(
        item,
        ThreadItemPayload::FileChange { .. }
            | ThreadItemPayload::CollabAgentToolCall { .. }
            | ThreadItemPayload::ImageView { .. }
            | ThreadItemPayload::ImageGeneration(_)
    )
    .then(|| item.id().to_owned());
    let mut cells: TranscriptCells = Vec::new();
    match item {
        item @ ThreadItemPayload::CodeModeOwned(_) => {
            cells.extend(super::thread_items_to_transcript_cells(
                /*thread_id*/ None,
                cwd,
                [item],
                super::RawReasoningVisibility::Hidden,
                /*config*/ None,
                metadata,
                image_origin,
            ))
        }
        ThreadItemPayload::CodeMode(item) => cells.push(Arc::new(history_cell::CodeModeFactCell(
            item, /*animations_enabled*/ false,
        ))),
        ThreadItemPayload::FileChange {
            id,
            changes,
            status,
        } => cells.push(history_cell::ToolOutputGroup::from_cells(
            history_cell::patch_apply_cells(
                id,
                file_update_changes_to_display(changes),
                cwd.as_path(),
                status,
            )
            .into_iter()
            .map(|cell| Arc::new(cell) as Arc<dyn history_cell::HistoryCell>),
        )),
        ThreadItemPayload::WebSearch(item) => {
            cells.push(Arc::new(history_cell::new_web_search_call(
                item.id,
                item.query,
                item.action.unwrap_or(WebSearchAction::Other),
            )))
        }
        ThreadItemPayload::ImageView { path, .. } => cells.push(Arc::new(
            history_cell::new_view_image_tool_call(path, image_origin),
        )),
        ThreadItemPayload::ImageGeneration(item) => {
            cells.push(Arc::new(history_cell::new_image_generation_call(
                item.id,
                &item.status,
                item.revised_prompt,
                item.saved_path,
            )));
        }
        item @ ThreadItemPayload::CollabAgentToolCall { .. } => {
            if let Some(cell) = multi_agents::tool_call_history_cell(
                &item, /*cached_spawn_request*/ None, metadata,
            ) {
                cells.push(Arc::new(cell));
            }
        }
        item @ ThreadItemPayload::SubAgentActivity { .. } => {
            if let Some(cell) = multi_agents::sub_agent_activity_history_cell(&item) {
                cells.push(Arc::from(cell));
            }
        }
        ThreadItemPayload::EnteredReviewMode { review, .. } => {
            cells.push(Arc::new(history_cell::new_review_status_line(format!(
                ">> Code review started: {review} <<"
            ))));
        }
        ThreadItemPayload::ExitedReviewMode { review, .. } => {
            cells.push(Arc::new(history_cell::new_review_status_line(format!(
                "<< Code review finished: {review} >>"
            ))));
        }
        ThreadItemPayload::ContextCompaction { id } => {
            cells.push(Arc::new(history_cell::CompactionCell { id, elapsed: None }));
        }
        // These items do not have richer history-cell presentations yet.
        ThreadItemPayload::HookPrompt { fragments, .. } => {
            if !fragments.is_empty() {
                cells.push(Arc::new(PlainHistoryCell::new(
                    fragments
                        .into_iter()
                        .map(|fragment| {
                            vec![
                                "hook prompt: ".dim(),
                                fragment.text.trim().to_string().into(),
                            ]
                            .into()
                        })
                        .collect(),
                )));
            }
        }
        item @ ThreadItemPayload::DynamicToolCall { .. } => {
            if let Some(cell) = history_cell::DynamicToolCallCell::from_item(item) {
                cells.push(Arc::new(cell));
            }
        }
        ThreadItemPayload::UserMessage { .. }
        | ThreadItemPayload::AgentMessage { .. }
        | ThreadItemPayload::FunctionCallOutput { .. }
        | ThreadItemPayload::ToolOutputTokens { .. }
        | ThreadItemPayload::Plan { .. }
        | ThreadItemPayload::Reasoning { .. }
        | ThreadItemPayload::Thought { .. }
        | ThreadItemPayload::Preamble { .. }
        | ThreadItemPayload::StepAdmission(_)
        | ThreadItemPayload::ToolInput(_)
        | ThreadItemPayload::Arrival(_)
        | ThreadItemPayload::CommandExecution { .. }
        | ThreadItemPayload::McpToolCall { .. }
        | ThreadItemPayload::Sleep(_) => {}
    }
    match call_id {
        Some(call_id) => {
            let cells: TranscriptCells = cells
                .into_iter()
                .map(|inner| {
                    if inner.tool_call_ids().is_empty() {
                        Arc::new(history_cell::ToolItemCell {
                            call_id: call_id.clone(),
                            inner,
                        }) as Arc<dyn history_cell::HistoryCell>
                    } else {
                        inner
                    }
                })
                .collect();
            if cells.len() > 1 {
                vec![history_cell::ToolOutputGroup::from_cells(cells)]
            } else {
                cells
            }
        }
        None => cells,
    }
}

#[cfg(test)]
#[path = "other_items_tests.rs"]
mod tests;
