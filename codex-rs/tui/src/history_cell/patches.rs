//! Patch summaries and image-tool transcript helpers.

use super::*;
use crate::history_cell::activity_parts::FoldState;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::terminal_hyperlinks::TerminalHyperlink;
use crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines;
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
    #[expect(clippy::expect_used, reason = "Continuations always own a file.")]
    pub(crate) fn with_activity_id(mut self, id: String) -> Self {
        self.call_id = id.clone();
        // The first file owns the call's token bill; sibling disclosures have separate IDs.
        self.activity_id = if self.continuation {
            let (path, _) = self.file.as_ref().expect("continuation has a file");
            format!("patch-file:{id}:{path:?}")
        } else {
            format!("patch:{id}")
        };
        self
    }

    pub(super) fn shares_heading_with(&self, other: &Self) -> bool {
        self.state == other.state
    }

    pub(super) fn is_active(&self) -> bool {
        self.state == PatchState::InProgress
    }

    pub(super) fn failure_label(&self) -> Option<&'static str> {
        match self.state {
            PatchState::Failed => Some("failed"),
            PatchState::Declined => Some("declined"),
            _ => None,
        }
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
            crate::render::header::paint(Line::from(vec!["• ".dim(), self.heading().bold()]))
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

    pub(super) fn edit_parts(
        &self,
        width: u16,
        expanded: &FoldState,
        full: bool,
    ) -> super::invocation::EditProjection {
        let all;
        let expanded = if full {
            all = expanded
                .iter()
                .cloned()
                .chain(self.activity_ids())
                .collect();
            &all
        } else {
            expanded
        };
        let mut parts = self.nested_summary_parts(width, expanded);
        if self.state != PatchState::Applied {
            return super::invocation::EditProjection::Other(parts);
        }
        // Native patch headers occupy exactly one line; continuations already omit it.
        if !self.continuation {
            parts.lines.remove(0);
            for control in &mut parts.controls {
                control.line -= 1;
            }
        }
        parts.header = None;
        super::invocation::EditProjection::Applied(parts)
    }
}

impl HistoryCell for PatchHistoryCell {
    fn nested_detail_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }
    fn nested_summary_parts(
        &self,
        width: u16,
        expanded: &FoldState,
    ) -> super::activity_parts::ActivityParts {
        let open = expanded.is_open(
            super::activity_parts::FoldGroup::Diff,
            &self.activity_id,
            false,
        );
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
                    group: super::activity_parts::FoldGroup::Diff,
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
        expanded: &FoldState,
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

/// Whether a server-reported path belongs to this machine's filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImagePathOrigin {
    Local,
    Remote,
}

#[derive(Debug, Clone)]
pub(crate) struct ViewImageHistoryCell {
    filename: String,
    path_label: String,
    origin: ImagePathOrigin,
    pub(crate) timestamp: Option<HistoryTimestamp>,
    link: std::sync::Arc<std::sync::OnceLock<Option<TerminalHyperlink>>>,
}

impl ViewImageHistoryCell {
    /// Off-thread retained-layout preparation owns filesystem validation, never admission or paint.
    pub(crate) fn link_local_image(&self) {
        if self.origin == ImagePathOrigin::Remote {
            return;
        }
        self.link.get_or_init(|| {
            let start = "Viewed image ".len();
            let end = start + crate::width::display_width(&self.path_label);
            AbsolutePathBuf::try_from(PathBuf::from(&self.path_label))
                .ok()
                .and_then(|path| TerminalHyperlink::viewed_image(start..end, path.as_path()))
        });
    }
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

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = plain_hyperlink_lines(self.display_lines(width));
        if let ([line], Some(link)) = (
            lines.as_mut_slice(),
            self.link.get().and_then(Option::as_ref),
        ) {
            let start = crate::width::display_width("• Viewed image ");
            if start < line.width() {
                line.hyperlinks.push(link.with_columns(start..line.width()));
            }
        }
        lines
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if width == 0 {
            return Vec::new();
        }
        let mut line = HyperlinkLine::new(Line::from(vec![
            "Viewed image ".bold(),
            self.path_label.clone().dim(),
        ]));
        line.hyperlinks
            .extend(self.link.get().and_then(Option::as_ref).cloned());
        adaptive_wrap_hyperlink_lines(
            &[line],
            RtOptions::new(usize::from(width))
                .initial_indent("• ".dim().into())
                .subsequent_indent("  ".into()),
        )
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![Line::from(format!("Viewed image {}", self.path_label))]
    }
}

pub(crate) fn new_view_image_tool_call(
    path: LegacyAppPathString,
    origin: ImagePathOrigin,
) -> ViewImageHistoryCell {
    let filename = path
        .to_inferred_path_uri()
        .and_then(|path| path.basename())
        .unwrap_or_else(|| path.render_for_ui());
    ViewImageHistoryCell {
        filename,
        origin,
        path_label: path.into_string(),
        timestamp: None,
        link: Default::default(),
    }
}

pub(crate) fn new_image_generation_call(
    call_id: String,
    status: &str,
    revised_prompt: Option<String>,
    saved_path: Option<AbsolutePathBuf>,
) -> PlainHistoryCell {
    super::image_generation::new_image_generation_call(call_id, status, revised_prompt, saved_path)
}
