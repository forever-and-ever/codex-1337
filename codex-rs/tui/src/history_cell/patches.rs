//! Patch summaries and image-tool transcript helpers.

use super::*;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use codex_ansi_escape::ansi_escape;
use codex_utils_path_uri::LegacyAppPathString;

#[cfg(test)]
#[path = "patches_tests.rs"]
mod tests;

#[derive(Debug)]
pub(crate) struct PatchHistoryCell {
    activity_id: String,
    call_id: String,
    path: PathBuf,
    change: FileChange,
    cwd: PathBuf,
    continuation: bool,
}

impl PatchHistoryCell {
    pub(crate) fn with_activity_id(mut self, id: String) -> Self {
        self.call_id = id.clone();
        // The first file owns the call's token bill; sibling disclosures have separate IDs.
        self.activity_id = if self.continuation {
            format!("patch-file:{id}:{:?}", self.path)
        } else {
            format!("patch:{id}")
        };
        self
    }

    fn summary(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = Vec::new();
        if !self.continuation {
            lines.push(Line::from(vec!["• ".dim(), "Edited".bold()]).into());
        }
        let mut path = crate::diff_render::display_path_for(&self.path, &self.cwd);
        if let FileChange::Update {
            move_path: Some(destination),
            ..
        } = &self.change
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
                    syntax_path: &self.path,
                    change: Some(&self.change),
                    qualifier: None,
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
            controls: vec![super::activity_parts::PartControl {
                id: self.activity_id.clone(),
                line: usize::from(!self.continuation),
                expanded: open,
            }],
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
        (!self.continuation).then(|| {
            super::activity_parts::ActivityHeader::at_end(&Line::from(vec![
                "• ".into(),
                "Edited".into(),
            ]))
        })
    }
    fn activity_ids(&self) -> Vec<String> {
        vec![self.activity_id.clone()]
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.summary(width)
    }

    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        Some(ActivityDisclosure::EditedFile {
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
        let mut detail = crate::file_change_view::FileChangeView {
            label: "".into(),
            syntax_path: &self.path,
            change: Some(&self.change),
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
    let id = uuid::Uuid::new_v4().to_string();
    let mut changes: Vec<_> = changes.into_iter().collect();
    changes.sort_by(|left, right| left.0.cmp(&right.0));
    changes
        .into_iter()
        .enumerate()
        .map(|(index, (path, change))| {
            PatchHistoryCell {
                activity_id: String::new(),
                call_id: String::new(),
                path,
                change,
                cwd: cwd.to_path_buf(),
                continuation: index > 0,
            }
            .with_activity_id(id.clone())
        })
        .collect()
}

pub(crate) fn new_patch_apply_failure(stderr: String) -> PatchFailureCell {
    PatchFailureCell {
        activity_id: format!("patch-failure:{}", uuid::Uuid::new_v4()),
        stderr,
    }
}

/// Failed patch attempts retain available diagnostics for local disclosure and full transcript.
#[derive(Debug)]
pub(crate) struct PatchFailureCell {
    activity_id: String,
    stderr: String,
}

impl HistoryCell for PatchFailureCell {
    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        (!self.stderr.trim().is_empty()).then_some(ActivityDisclosure::Generic)
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![self.activity_id.clone()]
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.transcript_hyperlink_lines(width);
        lines.truncate(1);
        lines
            .into_iter()
            .map(|line| super::activity_preview::clipped_line(line.line, width))
            .collect()
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = vec![Line::from("✘ Failed to apply patch".magenta().bold()).into()];
        let error = if self.stderr.trim().is_empty() {
            "(error details unavailable)"
        } else {
            &self.stderr
        };
        let mut diagnostics = ansi_escape(&error.replace('\t', "    ")).lines;
        for line in &mut diagnostics {
            for span in &mut line.spans {
                span.style = span.style.add_modifier(Modifier::DIM);
            }
        }
        lines.push(super::output_provenance::client_notice(
            "Stored result · exact capture unavailable",
        ));
        let mut diagnostics = plain_hyperlink_lines(diagnostics);
        super::output_provenance::mark_lines(
            &mut diagnostics,
            super::output_provenance::OutputProvenance::Client,
        );
        lines.extend(crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
            &diagnostics,
            RtOptions::new(usize::from(width).max(/*other*/ 1))
                .initial_indent("  └ ".dim().into())
                .subsequent_indent("    ".into()),
        ));
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from("Failed to apply patch")];
        lines.extend(plain_lines(ansi_escape(&self.stderr).lines));
        lines
    }

    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();

        // Failure title
        lines.push(Line::from("✘ Failed to apply patch".magenta().bold()));

        if !self.stderr.trim().is_empty() {
            let output = output_lines(
                Some(&CommandOutput::new(
                    /*exit_code*/ 1,
                    self.stderr.clone(),
                )),
                OutputLinesParams {
                    line_limit: TOOL_CALL_MAX_LINES,
                    only_err: true,
                    include_angle_pipe: true,
                    include_prefix: true,
                },
            );
            lines.extend(output.lines);
        }

        lines
    }
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
