//! User, assistant, reasoning, and streaming message history cells.
//! Completed reasoning is retained in the expanded transcript, not compact scrollback.

use super::markdown_render_cache::MarkdownRenderCache;
use super::*;
use crate::async_question_reply::AsyncQuestionReply;
use crate::style::accent_color_on;
use crate::terminal_hyperlinks::annotate_web_urls;
use crate::terminal_hyperlinks::lines_with_sources_eq;
use crate::terminal_hyperlinks::remap_source_wrapped_line;
use crate::warpaint::Bracket;
use crate::wrapping::url_preserving_wrap_options;
use crate::wrapping::word_wrap_line_with_source;
use std::borrow::Cow;

#[derive(Debug, Clone)]
pub(crate) struct UserHistoryCell {
    pub message: String,
    pub(crate) timestamp: Option<HistoryTimestamp>,
    pub text_elements: Vec<TextElement>,
    pub local_image_paths: Vec<PathBuf>,
    pub remote_image_urls: Vec<String>,
    pub(crate) prompt_identity: crate::prompt_order::UserPromptIdentity,
    pub(crate) presentation: UserMessagePresentation,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum UserMessagePresentation {
    #[default]
    Typed,
    Spoken,
    /// Render each submitted answer as Markdown beneath its question caption.
    AsyncReplies {
        /// Keep the decoded pairs so the two-row limit applies only to question captions.
        /// In the flattened message, an answer's own blockquotes can look like captions.
        replies: Vec<AsyncQuestionReply>,
        /// Capture the session directory so relative Markdown links resolve the same way on reflow.
        cwd: PathBuf,
    },
}

/// Remove CSI sequences and control characters, preserving tabs and newlines.
pub(crate) fn sanitize_user_text(text: Cow<'_, str>) -> Cow<'_, str> {
    fn sanitize_borrowed(text: &str) -> Cow<'_, str> {
        let mut remaining = Some(text);
        let mut spans = std::iter::from_fn(move || {
            let current = remaining.take()?;
            let mut escaped = false;
            let Some((prefix, suffix)) = current.split_once(|ch: char| {
                escaped = ch == '\x1b';
                escaped || ch.is_control() && !matches!(ch, '\n' | '\t')
            }) else {
                return Some(current);
            };

            remaining = if escaped && let Some(sequence) = suffix.strip_prefix('[') {
                sequence
                    .split_once(|ch: char| ('@'..='~').contains(&ch))
                    .map(|(_, tail)| tail)
            } else {
                Some(suffix)
            };
            Some(prefix)
        })
        .filter(|span| !span.is_empty());

        let first = spans.next().unwrap_or_default();
        let Some(second) = spans.next() else {
            return Cow::Borrowed(first);
        };

        Cow::Owned([first, second].into_iter().chain(spans).fold(
            String::with_capacity(text.len()),
            |mut acc, span| {
                acc.push_str(span);
                acc
            },
        ))
    }

    match text {
        Cow::Borrowed(text) => sanitize_borrowed(text),
        Cow::Owned(mut text) => match sanitize_borrowed(&text) {
            Cow::Owned(sanitized) => Cow::Owned(sanitized),
            Cow::Borrowed(retained) => {
                if retained.is_empty() {
                    text.clear();
                } else if retained.len() != text.len() {
                    // Cannot underflow because retained is a subslice of text.
                    // I'd normally assert that here but this crate denies
                    // clippy::expect_used.
                    let start = retained.as_ptr().addr() - text.as_ptr().addr();
                    let end = start + retained.len();

                    // Truncate before draining because truncate is constant
                    // time while drain is linear in the size of the receiver.
                    text.truncate(end);
                    drop(text.drain(..start));
                }
                Cow::Owned(text)
            }
        },
    }
}

/// Build logical lines for a user message with styled text elements.
///
/// This preserves explicit newlines while interleaving element spans and skips
/// malformed byte ranges instead of panicking during history rendering.
fn build_user_message_lines_with_elements(
    message: &str,
    elements: &[TextElement],
    style: Style,
    element_style: Style,
) -> Vec<Line<'static>> {
    let mut elements = elements.to_vec();
    elements.sort_by_key(|e| e.byte_range.start);
    let mut offset = 0usize;
    let mut raw_lines: Vec<Line<'static>> = Vec::new();
    for line_text in message.split('\n') {
        let line_start = offset;
        let line_end = line_start + line_text.len();
        let mut spans: Vec<Span<'static>> = Vec::new();
        // Track how much of the line we've emitted to interleave plain and styled spans.
        let mut cursor = line_start;
        for elem in &elements {
            let start = elem.byte_range.start.max(line_start);
            let end = elem.byte_range.end.min(line_end);
            if start >= end {
                continue;
            }
            let rel_start = start - line_start;
            let rel_end = end - line_start;
            // Guard against malformed UTF-8 byte ranges from upstream data; skip
            // invalid elements rather than panicking while rendering history.
            if !line_text.is_char_boundary(rel_start) || !line_text.is_char_boundary(rel_end) {
                continue;
            }
            let rel_cursor = cursor - line_start;
            if cursor < start
                && line_text.is_char_boundary(rel_cursor)
                && let Some(segment) = line_text.get(rel_cursor..rel_start)
            {
                spans.push(Span::from(segment.to_string()));
            }
            if let Some(segment) = line_text.get(rel_start..rel_end) {
                spans.push(Span::styled(segment.to_string(), element_style));
                cursor = end;
            }
        }
        let rel_cursor = cursor - line_start;
        if cursor < line_end
            && line_text.is_char_boundary(rel_cursor)
            && let Some(segment) = line_text.get(rel_cursor..)
        {
            spans.push(Span::from(segment.to_string()));
        }
        let line = if spans.is_empty() {
            Line::from(line_text.to_string()).style(style)
        } else {
            Line::from(spans).style(style)
        };
        raw_lines.push(line);
        // Split on '\n' so any '\r' stays in the line; advancing by 1 accounts
        // for the separator byte.
        offset = line_end + 1;
    }

    raw_lines
}

fn user_message_element_style(style: Style, accent: Color) -> Style {
    let unresolved_background = match style.bg {
        Some(Color::Indexed(index)) => index < 16,
        Some(Color::Rgb(..) | Color::Reset) | None => false,
        Some(_) => true,
    };
    if accent == Color::Reset || unresolved_background {
        style
    } else {
        style.fg(accent)
    }
}

impl UserHistoryCell {
    pub(crate) fn with_async_replies(
        mut self,
        replies: Vec<AsyncQuestionReply>,
        cwd: &Path,
    ) -> Self {
        if !replies.is_empty() {
            self.presentation = UserMessagePresentation::AsyncReplies {
                replies,
                cwd: cwd.to_owned(),
            };
        }
        self
    }

    pub(crate) fn is_spoken(&self) -> bool {
        matches!(self.presentation, UserMessagePresentation::Spoken)
    }

    pub(crate) fn uses_markdown(&self) -> bool {
        // Inline elements use byte ranges in the literal prompt.
        matches!(self.presentation, UserMessagePresentation::Typed) && self.text_elements.is_empty()
    }

    pub(crate) fn has_visible_content(&self) -> bool {
        !sanitize_user_text(Cow::Borrowed(&self.message))
            .trim()
            .is_empty()
            || !self.text_elements.is_empty()
            || !self.local_image_paths.is_empty()
            || !self.remote_image_urls.is_empty()
    }

    fn image_labels_not_in_message(&self) -> impl Iterator<Item = String> + '_ {
        // Composer images already have placeholders; command-line images may not.
        // Both kinds must keep an image-only user turn visible after submission.
        (1..=self.remote_image_urls.len() + self.local_image_paths.len())
            .map(local_image_label_text)
            .filter(|label| {
                !self
                    .text_elements
                    .iter()
                    .any(|element| element.placeholder(&self.message) == Some(label.as_str()))
            })
    }
}

impl HistoryCell for UserHistoryCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.timestamp
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        presentation::user_lines(self, width, presentation::Surface::Inline)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn retained_hyperlink_lines(&self, width: u16, _detailed: bool) -> Vec<HyperlinkLine> {
        presentation::user_lines(self, width, presentation::Surface::Transcript)
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.transcript_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let message = sanitize_user_text((&self.message).into());
        let mut lines = raw_lines_from_source(message.as_ref().trim_end_matches(['\r', '\n']));
        let mut image_labels = self.image_labels_not_in_message().peekable();
        if image_labels.peek().is_some() {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.extend(image_labels.map(Line::from));
        }
        lines
    }
}

/// Wrap a prompt body before the user-message gutter and background padding are added.
fn wrap_user_message(
    message: &str,
    text_elements: &[TextElement],
    style: Style,
    wrap_width: u16,
) -> Vec<HyperlinkLine> {
    let element_style = user_message_element_style(style, accent_color_on(style.bg));
    if message.is_empty() && text_elements.is_empty() {
        return Vec::new();
    }

    let wrap_options =
        RtOptions::new(usize::from(wrap_width)).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit);
    let logical_lines = if text_elements.is_empty() {
        let message_without_trailing_newlines = message.trim_end_matches(['\r', '\n']);
        message_without_trailing_newlines
            .split('\n')
            .map(|line| Line::from(line.to_owned()).style(style))
            .collect()
    } else {
        build_user_message_lines_with_elements(message, text_elements, style, element_style)
    };
    let mut wrapped = crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
        &annotate_web_urls(logical_lines),
        wrap_options,
    )
    .into_iter()
    .flat_map(|line| {
        if line.width() <= usize::from(wrap_width) {
            return vec![line];
        }

        // Terminal autowrap loses the message gutter and background. Explicitly split
        // oversized URL tokens while retaining their complete OSC-8 destination.
        let forced_lines = word_wrap_line_with_source(
            &line.line,
            url_preserving_wrap_options(RtOptions::new(usize::from(wrap_width)))
                .break_words(/*break_words*/ true),
        );
        remap_source_wrapped_line(&line, forced_lines)
    })
    .collect::<Vec<_>>();
    while wrapped.last().is_some_and(|line| {
        line.line
            .spans
            .iter()
            .all(|span| span.content.trim().is_empty())
    }) {
        wrapped.pop();
    }
    wrapped
}

#[derive(Debug)]
pub(crate) struct ReasoningSummaryCell {
    _header: String,
    content: String,
    /// Session cwd used to render local file links inside the reasoning body.
    cwd: PathBuf,
    transcript_only: bool,
    /// Persisted identity verifies page folds without comparing rendered reasoning text.
    source_item_id: Option<String>,
}

impl ReasoningSummaryCell {
    pub(crate) fn markdown_source(&self) -> &str {
        &self.content
    }

    pub(crate) fn set_source_item_id(&mut self, id: String) {
        self.source_item_id = Some(id);
    }

    pub(crate) fn source_item_id(&self) -> Option<&str> {
        self.source_item_id.as_deref()
    }

    pub(crate) fn is_transcript_only(&self) -> bool {
        self.transcript_only
    }

    /// Create a reasoning summary cell that will render local file links relative to the session
    /// cwd active when the summary was recorded.
    pub(crate) fn new(header: String, content: String, cwd: &Path, transcript_only: bool) -> Self {
        Self {
            _header: header,
            content,
            cwd: cwd.to_path_buf(),
            transcript_only,
            source_item_id: None,
        }
    }

    fn lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let lines = crate::markdown_render::render_markdown_lines_with_width_and_cwd(
            &self.content,
            crate::width::usable_content_width_u16(width, /*reserved_cols*/ 2),
            Some(self.cwd.as_path()),
        );
        let summary_style = Style::default().dim().italic();
        let summary_lines = lines
            .into_iter()
            .map(|mut line| {
                line.line.spans = line
                    .line
                    .spans
                    .into_iter()
                    .map(|span| span.patch_style(summary_style))
                    .collect();
                if let Some(source) = &mut line.source {
                    source.span_style = source.span_style.patch(summary_style);
                }
                line
            })
            .collect::<Vec<_>>();

        crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
            &summary_lines,
            RtOptions::new(width as usize)
                .initial_indent("• ".dim().into())
                .subsequent_indent("  ".into()),
        )
    }
}

impl HistoryCell for ReasoningSummaryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if self.transcript_only {
            Vec::new()
        } else {
            self.lines(width)
        }
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        if self.transcript_only {
            Vec::new()
        } else {
            raw_lines_from_source(self.markdown_source().trim())
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AgentMessageCell {
    lines: Vec<HyperlinkLine>,
    is_first_line: bool,
    pub(crate) timestamp: Option<HistoryTimestamp>,
    pub(crate) item_id: Option<String>,
}

impl AgentMessageCell {
    #[cfg(test)]
    pub(crate) fn new(lines: Vec<Line<'static>>, is_first_line: bool) -> Self {
        Self {
            lines: plain_hyperlink_lines(lines),
            is_first_line,
            timestamp: None,
            item_id: None,
        }
    }

    pub(crate) fn new_hyperlink_lines(lines: Vec<HyperlinkLine>, is_first_line: bool) -> Self {
        Self {
            lines,
            is_first_line,
            timestamp: None,
            item_id: None,
        }
    }
}

impl HistoryCell for AgentMessageCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.is_first_line.then_some(self.timestamp).flatten()
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        presentation::agent_lines(self, width, presentation::Surface::Inline)
    }

    fn retained_hyperlink_lines(&self, width: u16, _detailed: bool) -> Vec<HyperlinkLine> {
        presentation::agent_lines(self, width, presentation::Surface::Transcript)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(visible_lines(self.lines.clone()))
    }

    fn is_stream_continuation(&self) -> bool {
        !self.is_first_line
    }
}

/// A consolidated agent message cell that stores raw markdown source and re-renders from it.
///
/// After a stream finalizes, the `ConsolidateAgentMessage` handler in `App`
/// replaces the contiguous run of `AgentMessageCell`s with a single
/// `AgentMarkdownCell`. On terminal resize, `display_lines(width)` re-renders
/// from source via `append_markdown_agent`, producing correctly-sized tables
/// with box-drawing borders.
///
/// The cell snapshots `cwd` at construction so local file-link display remains aligned with the
/// session that produced the message. Reusing the current process cwd during reflow would make old
/// transcript content change meaning after a later `/cd` or resumed session.
///
/// Ordinary markdown caches its latest rich render. Visualization directives bypass that cache
/// because resolving their local file links depends on filesystem state that can change later.
#[derive(Debug)]
pub(crate) struct AgentMarkdownCell {
    pub(crate) timestamp: Option<HistoryTimestamp>,
    pub(crate) item_id: Option<String>,
    pub(crate) interrupted_item: Option<(String, String)>,
    markdown_source: String,
    copy_source: Option<String>,
    cwd: PathBuf,
    inline_visualization_context: Option<crate::inline_visualization::InlineVisualizationContext>,
    rendered_lines: Option<MarkdownRenderCache>,
    spoken_artifacts: bool,
    question_titles: Vec<std::ops::Range<usize>>,
}

impl AgentMarkdownCell {
    pub(crate) fn from_parsed_message(
        visible_markdown: String,
        source: String,
        cwd: &Path,
        context: Option<crate::inline_visualization::InlineVisualizationContext>,
        questions: &[codex_protocol::items::AsyncUserInputQuestion],
    ) -> Self {
        Self::new_with_inline_visualizations(visible_markdown, cwd, context)
            .with_questions(questions)
            .with_copy_source(Some(source))
    }

    pub(crate) fn with_copy_source(mut self, source: Option<String>) -> Self {
        self.copy_source = source;
        self
    }

    /// Create a finalized source-backed assistant message cell.
    ///
    /// `markdown_source` must be the raw source accumulated by the stream controller, not already
    /// wrapped terminal lines. Passing rendered lines here would make future resize reflow preserve
    /// stale wrapping instead of repairing it.
    #[cfg(test)]
    pub(crate) fn new(markdown_source: String, cwd: &Path) -> Self {
        Self::new_with_inline_visualizations(
            markdown_source,
            cwd,
            /*inline_visualization_context*/ None,
        )
    }

    pub(crate) fn new_with_inline_visualizations(
        markdown_source: String,
        cwd: &Path,
        inline_visualization_context: Option<
            crate::inline_visualization::InlineVisualizationContext,
        >,
    ) -> Self {
        let rendered_lines =
            (!crate::inline_visualization::contains_inline_visualization(&markdown_source))
                .then(MarkdownRenderCache::default);
        Self {
            interrupted_item: None,
            markdown_source,
            timestamp: None,
            item_id: None,
            copy_source: None,
            cwd: cwd.to_path_buf(),
            inline_visualization_context,
            rendered_lines,
            spoken_artifacts: false,
            question_titles: Vec::new(),
        }
    }

    pub(crate) fn with_questions(
        mut self,
        questions: &[codex_protocol::items::AsyncUserInputQuestion],
    ) -> Self {
        self.question_titles =
            super::async_questions::title_ranges(&self.markdown_source, questions);
        self
    }

    pub(crate) fn new_spoken(markdown_source: String, cwd: &Path) -> Self {
        let mut cell = Self::new_with_inline_visualizations(
            markdown_source,
            cwd,
            /*inline_visualization_context*/ None,
        );
        cell.spoken_artifacts = true;
        cell
    }
}

pub(super) fn normalize_whitespace_only_hyperlink_lines(
    mut lines: Vec<HyperlinkLine>,
) -> Vec<HyperlinkLine> {
    for line in &mut lines {
        if line
            .line
            .spans
            .iter()
            .all(|span| span.content.chars().all(char::is_whitespace))
        {
            line.line = Line::default().style(line.line.style);
            line.hyperlinks.clear();
            if let Some(source) = &mut line.source {
                source.prefix_bytes = 0;
                source.range.end = source.range.start;
            }
        }
    }
    lines
}

/// A message's words hang from its speaker's bracket.
fn spoken(mut lines: Vec<HyperlinkLine>, bracket: Bracket) -> Vec<HyperlinkLine> {
    super::output_provenance::hang(&mut lines, bracket);
    lines
}

impl AgentMarkdownCell {
    fn render_lines(&self, width: u16, surface: presentation::Surface) -> Vec<HyperlinkLine> {
        presentation::markdown_lines(self, width, surface)
    }
}

impl HistoryCell for AgentMarkdownCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.timestamp
    }
    fn copy_source(&self) -> Option<&str> {
        Some(self.copy_source.as_deref().unwrap_or(&self.markdown_source))
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.render_lines(width, presentation::Surface::Inline)
    }

    fn retained_hyperlink_lines(&self, width: u16, _detailed: bool) -> Vec<HyperlinkLine> {
        self.render_lines(width, presentation::Surface::Transcript)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        raw_lines_from_source(&self.markdown_source)
    }

    fn has_stable_transcript_height(&self) -> bool {
        self.rendered_lines.is_some()
    }
}

#[cfg(test)]
#[path = "messages_tests.rs"]
mod tests;

/// Transient active-cell representation of the mutable tail of an agent stream.
///
/// During streaming, lines that have not yet been committed to scrollback because they belong to
/// an in-progress table are displayed via this cell in the `active_cell` slot. It is replaced on
/// deltas that change the visible tail or retained source, and cleared when the stream finalizes.
#[derive(Clone, Debug, Eq)]
pub(crate) struct StreamingAgentTailCell {
    lines: Vec<HyperlinkLine>,
    is_first_line: bool,
    pub(crate) timestamp: Option<HistoryTimestamp>,
}

impl PartialEq for StreamingAgentTailCell {
    fn eq(&self, other: &Self) -> bool {
        self.is_first_line == other.is_first_line
            && lines_with_sources_eq(&self.lines, &other.lines)
    }
}

impl StreamingAgentTailCell {
    pub(crate) fn new(lines: Vec<HyperlinkLine>, is_first_line: bool) -> Self {
        Self {
            lines,
            is_first_line,
            timestamp: None,
        }
    }
}

impl HistoryCell for StreamingAgentTailCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.is_first_line.then_some(self.timestamp).flatten()
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        // Tail lines are already rendered at the controller's current stream width.
        // Re-wrapping them here can split table borders and produce malformed in-flight rows.
        spoken(
            normalize_whitespace_only_hyperlink_lines(prefix_hyperlink_lines(
                self.lines.clone(),
                agent_prefix(self.is_first_line),
                agent_prefix(/*first*/ false),
            )),
            Bracket::Response,
        )
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(/*width*/ u16::MAX))
    }

    fn is_stream_continuation(&self) -> bool {
        !self.is_first_line
    }
}
pub(crate) fn new_user_prompt(
    message: String,
    text_elements: Vec<TextElement>,
    local_image_paths: Vec<PathBuf>,
    remote_image_urls: Vec<String>,
) -> UserHistoryCell {
    UserHistoryCell {
        message,
        text_elements,
        local_image_paths,
        remote_image_urls,
        timestamp: None,
        prompt_identity: Default::default(),
        presentation: UserMessagePresentation::Typed,
    }
}

pub(crate) fn new_spoken_user_prompt(message: String) -> UserHistoryCell {
    let mut cell = new_user_prompt(message, Vec::new(), Vec::new(), Vec::new());
    cell.presentation = UserMessagePresentation::Spoken;
    cell
}

/// Retain a completed reasoning block in the expanded transcript only.
/// Activity headings belong to the live status row and must not accumulate in scrollback.
///
/// The helper snapshots `cwd` into the returned cell so local file links render the same way they
/// did while the turn was live, even if rendering happens after other app state has advanced. Part
/// boundaries are preserved so standalone empty placeholders can be removed without changing
/// literal HTML comments or bold-only summary content.
pub(crate) fn new_reasoning_summary_block(
    reasoning_parts: Vec<String>,
    cwd: &Path,
) -> Box<ReasoningSummaryCell> {
    let (header, content) = split_reasoning_summary_parts(&reasoning_parts);
    Box::new(ReasoningSummaryCell::new(
        header, content, cwd, /*transcript_only*/ true,
    ))
}

/// Split structured reasoning-summary parts into the status header and renderable content.
pub(crate) fn split_reasoning_summary_parts(reasoning_parts: &[String]) -> (String, String) {
    let mut leading_empty_part_header = None;
    let mut content_parts = Vec::with_capacity(reasoning_parts.len());

    for part in reasoning_parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let header_end = part.strip_prefix("**").and_then(|after_open| {
            after_open
                .find("**")
                .and_then(|close| (close > 0).then_some(close + 4))
        });
        let body = header_end.map_or(part, |header_end| &part[header_end..]);
        if body.trim() == "<!-- -->" {
            if content_parts.is_empty()
                && leading_empty_part_header.is_none()
                && let Some(header_end) = header_end
            {
                leading_empty_part_header = Some(part[..header_end].to_string());
            }
            continue;
        }

        content_parts.push(part);
    }

    let content = content_parts.join("\n\n");
    if content.is_empty() {
        return (leading_empty_part_header.unwrap_or_default(), content);
    }

    if let Some(after_open) = content.strip_prefix("**")
        && let Some(close) = after_open.find("**")
    {
        let after_close_idx = 2 + close + 2;
        let after_close = &content[after_close_idx..];
        if after_close.starts_with('\n') || after_close.starts_with('\r') {
            return (
                content[..after_close_idx].to_string(),
                after_close.to_string(),
            );
        }
    }

    (leading_empty_part_header.unwrap_or_default(), content)
}

mod presentation;

pub(crate) use presentation::is_message;
mod snapshot;
