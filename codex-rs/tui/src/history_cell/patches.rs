//! Patch summaries and image-tool transcript helpers.

use super::*;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use codex_app_server_protocol::PatchApplyStatus;
use codex_utils_path_uri::LegacyAppPathString;

#[cfg(test)]
#[path = "patches_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PatchState {
    InProgress,
    Applied,
    NoChanges,
    Failed,
    Declined,
}

#[derive(Clone, Debug)]
pub(crate) struct PatchHistoryCell {
    activity_id: String,
    call_id: String,
    file: Option<(PathBuf, FileChange)>,
    cwd: PathBuf,
    continuation: bool,
    state: PatchState,
}

impl PatchHistoryCell {
    pub(crate) fn with_activity_id(mut self, id: String) -> Self {
        self.call_id = id.clone();
        // The first file owns the call's token bill; sibling disclosures have separate IDs.
        self.activity_id = if self.continuation {
            format!(
                "patch-file:{id}:{:?}",
                self.file.as_ref().expect("continuation has a file").0
            )
        } else {
            format!("patch:{id}")
        };
        self
    }

    pub(super) fn is_active(&self) -> bool {
        self.state == PatchState::InProgress
    }

    pub(super) fn revise(&self, newer: &Self) -> Option<Self> {
        (self.call_id == newer.call_id
            && (self.file.is_none()
                || newer.file.is_none()
                || self.file.as_ref().map(|(path, _)| path)
                    == newer.file.as_ref().map(|(path, _)| path)))
        .then(|| {
            if newer.is_active() && !self.is_active() {
                self.clone()
            } else if newer.file.is_none() {
                // An empty terminal map settles the request without claiming its files changed.
                let mut updated = self.clone();
                updated.state = newer.state;
                updated
            } else {
                newer.clone()
            }
        })
    }

    fn heading(&self) -> &'static str {
        match self.state {
            PatchState::InProgress => "Patch application pending",
            PatchState::Applied => "Edited",
            PatchState::NoChanges => "No changes",
            PatchState::Failed => "Failed to apply patch",
            PatchState::Declined => "Patch application declined",
        }
    }

    fn header_line(&self) -> Line<'static> {
        if self.state == PatchState::Failed {
            Line::from(vec!["✘ ".red(), self.heading().red().bold()])
        } else {
            Line::from(vec!["• ".dim(), self.heading().bold()])
        }
    }

    fn summary(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = Vec::new();
        if !self.continuation {
            lines.push(self.header_line().into());
        }
        let Some((file_path, change)) = &self.file else {
            return lines;
        };
        let mut path = crate::diff_render::display_path_for(file_path, &self.cwd);
        if let FileChange::Update {
            move_path: Some(destination),
            ..
        } = change
        {
            path.push_str(&format!(
                " → {}",
                crate::diff_render::display_path_for(destination, &self.cwd)
            ));
        }
        let width = usize::from(width).max(/*other*/ 1);
        lines.extend(crate::terminal_hyperlinks::prefix_hyperlink_lines(
            vec![
                crate::file_change_view::FileChangeView {
                    label: path.into(),
                    syntax_path: file_path,
                    change: Some(change),
                    qualifier: (self.state != PatchState::Applied).then_some("requested"),
                }
                .summary(
                    width.saturating_sub(usize::from(crate::transcript_geometry::CHILD_INDENT)),
                ),
            ],
            crate::transcript_geometry::CHILD_PREFIX.into(),
            crate::transcript_geometry::CHILD_PREFIX.into(),
        ));
        lines
    }
}

impl HistoryCell for PatchHistoryCell {
    fn nested_detail_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }
    fn nested_summary_parts(
        &self,
        width: u16,
        expanded: &std::collections::HashSet<String>,
    ) -> super::activity_parts::ActivityParts {
        let open = expanded.contains(&self.activity_id);
        super::activity_parts::ActivityParts {
            lines: if open {
                self.display_hyperlink_lines(width)
            } else {
                self.summary(width)
            },
            header: self.activity_header(),
            output: None,
            controls: self
                .file
                .as_ref()
                .map(|_| super::activity_parts::PartControl {
                    id: self.activity_id.clone(),
                    line: usize::from(!self.continuation),
                    expanded: open,
                })
                .into_iter()
                .collect(),
        }
    }
    fn nested_activity_parts(
        &self,
        width: u16,
        expanded: &std::collections::HashSet<String>,
    ) -> Option<super::activity_parts::ActivityParts> {
        Some(self.nested_summary_parts(width, expanded))
    }
    fn tool_call_ids(&self) -> Vec<&str> {
        vec![&self.call_id]
    }
    fn outer_call_id(&self) -> Option<&str> {
        (!self.call_id.is_empty()).then_some(self.call_id.as_str())
    }
    fn activity_header(&self) -> Option<super::activity_parts::ActivityHeader> {
        (!self.continuation)
            .then(|| super::activity_parts::ActivityHeader::at_end(&self.header_line()))
    }
    fn activity_ids(&self) -> Vec<String> {
        vec![self.activity_id.clone()]
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.summary(width)
    }

    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        self.file.as_ref().map(|_| ActivityDisclosure::EditedFile {
            header: !self.continuation,
        })
    }

    fn is_stream_continuation(&self) -> bool {
        self.continuation
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.summary(width);
        let Some((path, change)) = &self.file else {
            return lines;
        };
        let mut detail = crate::file_change_view::FileChangeView {
            label: "".into(),
            syntax_path: path,
            change: Some(change),
            qualifier: None,
        }
        .body(
            usize::from(width)
                .saturating_sub(usize::from(crate::transcript_geometry::CHILD_INDENT)),
        );
        super::output_provenance::mark_lines(
            &mut detail,
            super::output_provenance::OutputProvenance::Client,
        );
        lines.extend(crate::terminal_hyperlinks::prefix_hyperlink_lines(
            detail,
            crate::transcript_geometry::CHILD_PREFIX.into(),
            crate::transcript_geometry::CHILD_PREFIX.into(),
        ));
        lines
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(visible_lines(
            self.display_hyperlink_lines(RAW_DIFF_SUMMARY_WIDTH as u16),
        ))
    }
}
/// Each file owns its disclosure and source text; replay uses the same sorted identities.
pub(crate) fn new_patch_events(
    changes: HashMap<PathBuf, FileChange>,
    cwd: &Path,
) -> Vec<PatchHistoryCell> {
    patch_apply_cells(
        uuid::Uuid::new_v4().to_string(),
        changes,
        cwd,
        PatchApplyStatus::Completed,
    )
}

pub(crate) fn patch_apply_cells(
    id: String,
    changes: HashMap<PathBuf, FileChange>,
    cwd: &Path,
    status: PatchApplyStatus,
) -> Vec<PatchHistoryCell> {
    let state = match status {
        PatchApplyStatus::InProgress => PatchState::InProgress,
        PatchApplyStatus::Completed if changes.is_empty() => PatchState::NoChanges,
        PatchApplyStatus::Completed => PatchState::Applied,
        PatchApplyStatus::Failed => PatchState::Failed,
        PatchApplyStatus::Declined => PatchState::Declined,
    };
    let mut changes: Vec<_> = changes.into_iter().collect();
    changes.sort_by(|left, right| left.0.cmp(&right.0));
    let files: Vec<_> = if changes.is_empty() {
        vec![None]
    } else {
        changes.into_iter().map(Some).collect()
    };
    files
        .into_iter()
        .enumerate()
        .map(|(index, file)| {
            PatchHistoryCell {
                activity_id: String::new(),
                call_id: String::new(),
                file,
                cwd: cwd.to_path_buf(),
                continuation: index > 0,
                state,
            }
            .with_activity_id(id.clone())
        })
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) struct ViewImageHistoryCell {
    filename: String,
    path_label: String,
    pub(crate) timestamp: Option<HistoryTimestamp>,
}

impl HistoryCell for ViewImageHistoryCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.timestamp
    }
    fn nested_summary_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut nested = self.clone();
        nested.timestamp = None;
        nested.compact_hyperlink_lines(width)
    }
    fn nested_detail_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut nested = self.clone();
        nested.timestamp = None;
        let mut lines = nested.transcript_hyperlink_lines(width);
        super::output_provenance::mark_lines(
            &mut lines,
            super::output_provenance::OutputProvenance::Client,
        );
        lines
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let line = vec![
            agent_prefix(/*first*/ true),
            "Viewed image ".bold(),
            self.filename.replace(['\n', '\r', '\t'], " ").dim(),
        ]
        .into();
        vec![truncate_line_with_ellipsis_if_overflow(
            line,
            width as usize,
        )]
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        PrefixedWrappedHistoryCell::new(
            Line::from(vec!["Viewed image ".bold(), self.path_label.clone().dim()]),
            vec![agent_prefix(/*first*/ true)],
            "  ",
        )
        .display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![Line::from(format!("Viewed image {}", self.path_label))]
    }
}

pub(crate) fn new_view_image_tool_call(path: LegacyAppPathString) -> ViewImageHistoryCell {
    let filename = path
        .to_inferred_path_uri()
        .and_then(|path| path.basename())
        .unwrap_or_else(|| path.render_for_ui());
    ViewImageHistoryCell {
        filename,
        path_label: path.into_string(),
        timestamp: None,
    }
}

pub(crate) fn new_image_generation_call(
    call_id: String,
    status: &str,
    revised_prompt: Option<String>,
    saved_path: Option<AbsolutePathBuf>,
) -> PlainHistoryCell {
    let detail = revised_prompt.unwrap_or(call_id);
    let heading = if status == "failed" {
        vec!["✗ ".red().bold(), "Image generation failed".bold()].into()
    } else {
        vec!["• ".dim(), "Generated Image:".bold()].into()
    };
    let mut lines: Vec<Line<'static>> = vec![heading, vec!["  └ ".dim(), detail.dim()].into()];
    if let Some(saved_path) = saved_path {
        let saved_path = Url::from_file_path(saved_path.as_path())
            .map(|url| url.to_string())
            .unwrap_or_else(|_| saved_path.display().to_string());
        lines.push(vec!["  └ ".dim(), "Saved to: ".dim(), saved_path.into()].into());
    }

    PlainHistoryCell::new(lines)
}
