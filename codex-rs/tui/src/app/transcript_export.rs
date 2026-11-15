//! Complete, Markdown-preserving conversation exports.

mod continuations;
pub(super) use continuations::ActiveExport;
pub(crate) use continuations::PreparedExport;

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::Turn;
use codex_protocol::ThreadId;
use codex_protocol::models::local_image_label_text;

use super::App;
use crate::app_event::TranscriptExportDestination;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::is_history_pagination_unsupported;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::ProposedPlanCell;
use crate::history_cell::ReasoningSummaryCell;
use crate::history_cell::SessionInfoCell;
use crate::history_cell::UserHistoryCell;
use crate::history_cell::raw_lines_from_source;
use crate::legacy_core::config::Config;
use crate::thread_transcript::RawReasoningVisibility;
use crate::thread_transcript::thread_items_to_transcript_cells;

#[cfg(test)]
fn write_transcript(cwd: &Path, requested_path: &Path, markdown: &str) -> Result<PathBuf, String> {
    let (file, path) = continuations::prepare_transcript(cwd, requested_path, markdown)?;
    file.persist_noclobber(&path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    Ok(path)
}

impl App {
    pub(super) fn export_transcript(
        &mut self,
        server: &AppServerSession,
        destination: TranscriptExportDestination,
    ) -> Result<(), String> {
        self.export_transcript_continuation(server, destination)
    }
}

pub(super) async fn load_export_transcript(
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    visibility: RawReasoningVisibility,
    config: Option<&Config>,
    visible_transcript: Vec<Arc<dyn HistoryCell>>,
    metadata: &(dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata + Sync),
    progress: &(dyn Fn() + Send + Sync),
    image_origin: crate::history_cell::ImagePathOrigin,
) -> Result<Vec<Arc<dyn HistoryCell>>, String> {
    let Some(thread) =
        continuations::load_export_thread(handle, thread_id, progress, image_origin).await?
    else {
        return Ok(visible_transcript);
    };
    let mut cells: Vec<Arc<dyn HistoryCell>> = Vec::new();
    for item in visible_export_items(thread.turns) {
        if let Some(cell) = export_activity_cell(&item) {
            cells.push(Arc::new(cell));
        } else {
            cells.extend(thread_items_to_transcript_cells(
                Some(thread_id),
                &thread.cwd,
                [item],
                visibility,
                config,
                metadata,
                image_origin,
            ));
        }
    }
    Ok(cells)
}

fn export_activity_cell(item: &ThreadItemPayload) -> Option<PlainHistoryCell> {
    let lines = match item {
        ThreadItemPayload::FileChange {
            changes, status, ..
        } => {
            let mut lines =
                vec![format!("file changes: {status:?} · {} changes", changes.len()).into()];
            for change in changes {
                lines.push(format!("{:?}: {}", change.kind, change.path).into());
                lines.extend(change.diff.lines().map(|line| line.to_string().into()));
            }
            lines
        }
        ThreadItemPayload::McpToolCall {
            server,
            tool,
            status,
            arguments,
            result,
            error,
            ..
        } => {
            let mut lines =
                vec![format!("mcp tool: {server}/{tool}({arguments}) · {status:?}").into()];
            if let Some(result) = result {
                for content in &result.content {
                    match serde_json::from_value::<rmcp::model::ContentBlock>(content.clone()) {
                        Ok(rmcp::model::ContentBlock::Text(text)) => {
                            lines.extend(raw_lines_from_source(&text.text));
                        }
                        Ok(rmcp::model::ContentBlock::Image(_)) => {
                            lines.push("Returned image".into());
                        }
                        Ok(rmcp::model::ContentBlock::Audio(_)) => {
                            lines.push("<audio content>".into());
                        }
                        Ok(rmcp::model::ContentBlock::Resource(_)) => {
                            let uri = content
                                .pointer("/resource/uri")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("<unknown embedded resource>");
                            lines.push(format!("embedded resource: {uri}").into());
                        }
                        Ok(rmcp::model::ContentBlock::ResourceLink(link)) => {
                            lines.push(format!("link: {}", link.uri).into());
                        }
                        _ => lines.push(content.to_string().into()),
                    }
                }
                if let Some(content) = &result.structured_content {
                    lines.push(format!("structured result: {content}").into());
                }
            }
            if let Some(error) = error {
                lines.extend(raw_lines_from_source(&format!("error: {}", error.message)));
            }
            lines
        }
        _ => return None,
    };
    Some(PlainHistoryCell::new(lines))
}

fn visible_export_items(turns: Vec<Turn>) -> Vec<ThreadItemPayload> {
    let mut visible = Vec::new();
    let mut review_mode = false;
    let mut previous_turn = None;

    for turn in turns {
        let hidden_nested_review_turn = previous_turn.as_ref().is_some_and(|previous| {
            crate::app_backtrack::is_hidden_nested_review_turn(previous, &turn)
        });
        for item in turn.item_payloads().cloned() {
            match item {
                ThreadItemPayload::EnteredReviewMode { .. }
                | ThreadItemPayload::ExitedReviewMode { .. } => {
                    review_mode = matches!(item, ThreadItemPayload::EnteredReviewMode { .. });
                    visible.push(item);
                }
                ThreadItemPayload::UserMessage { .. }
                    if review_mode || hidden_nested_review_turn => {}
                _ => visible.push(item),
            }
        }
        previous_turn = Some(turn);
    }

    visible
}

/// Markdown has no provenance fill; frame the shared source projection explicitly.
fn context_export_lines(cell: &dyn HistoryCell) -> Option<Vec<ratatui::text::Line<'static>>> {
    use crate::history_cell::ArrivalCell;
    use crate::history_cell::PreambleCell;
    use crate::history_cell::output_provenance::OutputMode;
    use crate::history_cell::output_provenance::OutputProvenance;
    use crate::transcript_view::FeedEntry;

    let (title, sources) = if let Some(preamble) = cell.as_any().downcast_ref::<PreambleCell>() {
        (
            preamble.feed_title(),
            preamble.source_lines(OutputMode::Raw),
        )
    } else {
        let arrival = cell.as_any().downcast_ref::<ArrivalCell>()?;
        (arrival.feed_title(), arrival.source_lines(OutputMode::Raw))
    };
    let mut lines = vec!["[Client annotation]".into(), title];
    let mut previous = None;
    for line in sources {
        let provenance = line
            .source
            .as_ref()
            .map_or(OutputProvenance::Unspecified, |source| {
                source.output_provenance
            });
        let label = match provenance {
            OutputProvenance::Captured => Some("[Captured context]"),
            OutputProvenance::Reasoning => Some("[Returned reasoning]"),
            OutputProvenance::Formatted => Some("[Context with control characters escaped]"),
            OutputProvenance::Client | OutputProvenance::ClientDiff => {
                Some("[Client annotation or representation]")
            }
            OutputProvenance::Definitions(OutputMode::Raw) => {
                Some("[Submitted tool definitions · client JSON]")
            }
            OutputProvenance::Definitions(OutputMode::Fmt) => {
                Some("[Submitted tool definitions · client JSON with control characters escaped]")
            }
            OutputProvenance::Unspecified => None,
        };
        if let Some(label) = label.filter(|_| previous != Some(provenance)) {
            lines.push(label.into());
            previous = Some(provenance);
        }
        lines.push(line.line);
    }
    Some(lines)
}

fn render_markdown_transcript(cells: &[Arc<dyn HistoryCell>]) -> Result<String, String> {
    let mut markdown = String::from("# Codex conversation\n");
    for cell in cells {
        let lines = if let Some(user) = cell.as_any().downcast_ref::<UserHistoryCell>() {
            let (message, _) =
                crate::prompt_request::extract_prompt_request_with_offset(&user.message);
            let message = crate::history_cell::sanitize_user_text(message.into());
            let mut lines = raw_lines_from_source(&message);
            let image_count = user.local_image_paths.len() + user.remote_image_urls.len();
            let image_labels = (0..image_count)
                .map(|index| local_image_label_text(index + 1))
                .filter(|label| !message.contains(label))
                .collect::<Vec<_>>();
            if !image_labels.is_empty() && !lines.is_empty() {
                lines.push("".into());
            }
            lines.extend(image_labels.into_iter().map(Into::into));
            lines
        } else if let Some(reasoning) = cell.as_any().downcast_ref::<ReasoningSummaryCell>() {
            raw_lines_from_source(reasoning.markdown_source().trim())
        } else if let Some(lines) = context_export_lines(cell.as_ref()) {
            lines
        } else {
            cell.raw_lines()
        };
        if lines.is_empty()
            || cell.as_any().is::<SessionInfoCell>()
            || cell.as_any().is::<PlainHistoryCell>()
                && lines.first().is_some_and(|line| {
                    let text = line.to_string();
                    [
                        "• Saved conversation to ",
                        "• Copied conversation to clipboard",
                        "• Copy unconfirmed; /export saves chat",
                        "• Export canceled.",
                        "■ Export failed: ",
                        "■ Copy failed: ",
                    ]
                    .iter()
                    .any(|prefix| text.starts_with(prefix))
                })
        {
            continue;
        }
        let (heading, indent) = if cell.as_any().is::<UserHistoryCell>() {
            ("User", false)
        } else if cell.as_any().is::<AgentMarkdownCell>() {
            ("Assistant", false)
        } else if cell.as_any().is::<ProposedPlanCell>() {
            ("Plan", false)
        } else if cell.as_any().is::<ReasoningSummaryCell>() {
            ("Reasoning", false)
        } else {
            ("Activity", true)
        };
        markdown.push_str(&format!("\n## {heading}\n\n"));
        for line in lines {
            if indent && line.spans.iter().any(|span| !span.content.is_empty()) {
                markdown.push_str("    ");
            }
            for span in line.spans {
                markdown.push_str(&crate::history_cell::sanitize_user_text(span.content));
            }
            markdown.push('\n');
        }
    }
    if markdown != "# Codex conversation\n" {
        Ok(markdown)
    } else {
        Err("No conversation content to export.".to_string())
    }
}

#[cfg(test)]
#[path = "transcript_export_tests.rs"]
mod tests;
