//! Producer evidence stays scoped to its MCP call; this adapter never opens its URIs.

use crate::history_cell::activity_parts::FoldState;
use codex_protocol::mcp_file_changes::FileChangeReceipt;
use codex_protocol::mcp_file_changes::FileChangeRecord;
use codex_protocol::mcp_file_changes::FilePublication;
use codex_protocol::mcp_file_changes::PublicationOutcome;
use codex_protocol::mcp_file_changes::SummaryReason;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::path::PathBuf;

use super::activity_parts::PartControl;
use crate::diff_model::FileChange;
use crate::file_change_view::FileChangeView;
use crate::terminal_hyperlinks::HyperlinkLine;

#[derive(Clone, Copy)]
pub(super) enum FileChangeDetail {
    Summary,
    Full,
    Disclosed,
}

#[derive(Clone, Debug)]
pub(super) struct ReceiptView {
    outcome: PublicationOutcome,
    omitted_files: u64,
    files: Vec<ReceiptFile>,
}

#[derive(Clone, Debug)]
struct ReceiptFile {
    label: String,
    syntax_path: PathBuf,
    change: Option<FileChange>,
    qualifier: Option<&'static str>,
}

impl ReceiptView {
    #[expect(
        clippy::expect_used,
        reason = "Receipt admission guarantees at least one URI per operation"
    )]
    pub(super) fn new(receipt: FileChangeReceipt) -> Self {
        let outcome = receipt.outcome();
        let omitted_files = receipt.omitted_files();
        let files = receipt
            .into_files()
            .into_iter()
            .map(|file| {
                let (publication, _, before, after) = file.identities();
                // Labels are display-only: never map the producer's namespace onto the host.
                let path = before.or(after).expect("admitted operation has a path");
                let label = match (before, after) {
                    (Some(before), Some(after)) if before != after => {
                        format!("{} → {}", display_uri(before), display_uri(after))
                    }
                    _ => display_uri(path),
                };
                let syntax_path = PathBuf::from(display_uri(after.unwrap_or(path)));
                let qualifier = match &file {
                    FileChangeRecord::Summary { reason, .. } => Some(match reason {
                        SummaryReason::Binary => "binary; full diff unavailable",
                        SummaryReason::NonUtf8 => "non-UTF-8; full diff unavailable",
                        SummaryReason::Oversized => "too large; full diff unavailable",
                    }),
                    FileChangeRecord::PathOnly { publication, .. } => Some(match publication {
                        FilePublication::NotApplied => "not applied; content unavailable",
                        FilePublication::Unknown => "publication unknown; content unavailable",
                        FilePublication::Applied | FilePublication::Preview => {
                            unreachable!("admitted path-only publication")
                        }
                    }),
                    FileChangeRecord::Text { .. } => match publication {
                        FilePublication::Preview => Some("preview"),
                        FilePublication::Applied => None,
                        FilePublication::NotApplied | FilePublication::Unknown => {
                            unreachable!("admitted text publication")
                        }
                    },
                };
                let change = match file {
                    FileChangeRecord::Text { unified_diff, .. } => Some(FileChange::Update {
                        unified_diff,
                        move_path: None,
                    }),
                    FileChangeRecord::Summary { .. } | FileChangeRecord::PathOnly { .. } => None,
                };
                ReceiptFile {
                    label,
                    syntax_path,
                    change,
                    qualifier,
                }
            })
            .collect();
        Self {
            outcome,
            omitted_files,
            files,
        }
    }

    pub(super) fn lines(
        &self,
        width: u16,
        detail: FileChangeDetail,
        call_id: &str,
        expanded: &FoldState,
    ) -> (Vec<HyperlinkLine>, Vec<PartControl>) {
        let outcome = match self.outcome {
            PublicationOutcome::Applied if self.files.is_empty() && self.omitted_files == 0 => {
                "No changes"
            }
            PublicationOutcome::Applied => "Edited",
            PublicationOutcome::Preview => "Preview — not applied",
            PublicationOutcome::NotApplied => "Not applied",
            PublicationOutcome::Partial => "Partially applied",
            PublicationOutcome::Unknown => "Changes uncertain",
        };
        let options = crate::wrapping::RtOptions::new(usize::from(width).max(/*other*/ 1));
        let mut lines = crate::wrapping::word_wrap_lines(
            [crate::render::header::leading(outcome, Line::default())],
            options.clone(),
        )
        .into_iter()
        .map(HyperlinkLine::from)
        .collect::<Vec<_>>();
        let mut controls = Vec::new();
        let file_width = usize::from(width)
            .saturating_sub(usize::from(crate::transcript_geometry::ENTRY_INDENT));
        for (index, file) in self.files.iter().enumerate() {
            let view = FileChangeView {
                label: file.label.as_str().into(),
                syntax_path: &file.syntax_path,
                change: file.change.as_ref(),
                qualifier: file.qualifier,
            };
            let id = format!("mcp-file:{call_id}:{index}");
            let is_expanded = matches!(detail, FileChangeDetail::Full)
                || expanded.is_open(super::activity_parts::FoldGroup::Diff, &id, false);
            if matches!(detail, FileChangeDetail::Disclosed)
                && matches!(&file.change, Some(FileChange::Update { unified_diff, .. }) if !unified_diff.is_empty())
            {
                controls.push(PartControl {
                    group: super::activity_parts::FoldGroup::Diff,
                    id,
                    line: lines.len(),
                    expanded: is_expanded,
                });
            }
            let mut file_lines = vec![view.summary(file_width)];
            if is_expanded {
                let mut detail = view.body(file_width);
                super::output_provenance::mark_lines(
                    &mut detail,
                    super::output_provenance::OutputProvenance::Client,
                );
                file_lines.extend(detail);
            }
            lines.extend(crate::transcript_geometry::gutter_lines(
                file_lines,
                crate::transcript_geometry::ENTRY_INDENT,
            ));
        }
        if self.omitted_files > 0 {
            lines.extend(
                crate::wrapping::word_wrap_lines(
                    [Line::from(
                        format!(
                            "{} additional files omitted; full diffs unavailable",
                            self.omitted_files
                        )
                        .dim(),
                    )],
                    options.clone(),
                )
                .into_iter()
                .map(HyperlinkLine::from),
            );
        } else if self.files.is_empty() && self.outcome == PublicationOutcome::Unknown {
            lines.extend(
                crate::wrapping::word_wrap_lines(
                    [Line::from(
                        "Paths and effects unavailable; changes may have occurred.".dim(),
                    )],
                    options,
                )
                .into_iter()
                .map(HyperlinkLine::from),
            );
        }
        (lines, controls)
    }
}

#[expect(
    clippy::expect_used,
    reason = "Only admitted file:// URIs with an absolute path reach this adapter"
)]
fn display_uri(uri: &str) -> String {
    let tail = uri.strip_prefix("file://").expect("admitted file URI");
    let (authority, path) = tail.split_once('/').expect("admitted absolute URI path");
    let path = urlencoding::decode_binary(path.as_bytes());
    let path = String::from_utf8_lossy(&path);
    if authority.is_empty() {
        format!("/{path}")
    } else {
        format!("//{authority}/{path}")
    }
}
