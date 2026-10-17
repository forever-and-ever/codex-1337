//! Publication holdback uses the ordinary parser, never a streaming Markdown dialect.

use super::StreamCore;
use crate::history_cell::HistoryRenderMode;
use crate::markdown::render_markdown_agent_with_list_spacing;
use crate::terminal_hyperlinks::HyperlinkLine;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

#[derive(Default)]
pub(super) struct Publication {
    scanned: usize,
    partial: bool,
    unresolved_reference: Option<usize>,
    reconcile: bool,
    raw_source: usize,
    raw_rows: usize,
}

impl Publication {
    pub(super) fn reset_partial(&mut self) {
        *self = Self::default();
    }
    fn hold_or_drain(
        &mut self,
        core: &mut StreamCore,
    ) -> (Vec<HyperlinkLine>, Option<Vec<HyperlinkLine>>) {
        if !core.state.collector.pending_source().is_empty() && self.partial {
            return (Vec::new(), None);
        }
        self.partial = false;
        (self.drain(core), Some(Vec::new()))
    }

    pub(super) fn observe(&mut self, core: &StreamCore, delta: &str) {
        // A late pipe or setext underline is allowed to revise a previously plain paragraph.
        if self.partial
            && (core.state.collector.pending_source().contains('|')
                || delta.contains('|')
                || delta.lines().any(|line| {
                    let line = line.trim();
                    !line.is_empty()
                        && (line.bytes().all(|ch| ch == b'=') || line.bytes().all(|ch| ch == b'-'))
                }))
        {
            self.reconcile = true;
        }
    }

    pub(super) fn needs_rewrap(&self, core: &StreamCore) -> bool {
        self.reconcile && core.state.collector.pending_source().is_empty()
    }

    fn prepare_raw(
        &mut self,
        core: &mut StreamCore,
    ) -> (Vec<HyperlinkLine>, Option<Vec<HyperlinkLine>>) {
        let source = core.state.collector.source();
        let mut rows = Vec::new();
        let mut tail = Vec::new();
        for line in source[self.raw_source..].split_inclusive('\n') {
            let complete = line.ends_with('\n');
            let text = line.strip_suffix('\n').unwrap_or(line);
            let safe = if complete {
                text.len()
            } else {
                text.char_indices()
                    .rev()
                    .find(|(_, ch)| ch.is_whitespace())
                    .map_or(0, |(offset, ch)| offset + ch.len_utf8())
            };
            if safe == 0 && !complete {
                break;
            }
            let raw = HyperlinkLine::new(ratatui::text::Line::from(text[..safe].to_owned()));
            let mut wrapped = match core.width {
                Some(width) => crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
                    &[raw],
                    crate::wrapping::RtOptions::new(width),
                ),
                None => vec![raw],
            };
            if !complete {
                tail.extend(wrapped.pop());
            }
            let count = wrapped.len();
            rows.extend(wrapped.into_iter().skip(self.raw_rows));
            if complete {
                self.raw_source += line.len();
                self.raw_rows = 0;
            } else {
                self.raw_rows = count;
            }
        }
        core.state.clear_queue();
        core.enqueued_stable_len = core.render.lines.len();
        core.emitted_stable_len = core.render.lines.len();
        (rows, Some(tail))
    }

    pub(super) fn prepare(
        &mut self,
        core: &mut StreamCore,
    ) -> (Vec<HyperlinkLine>, Option<Vec<HyperlinkLine>>) {
        if core.render_mode == HistoryRenderMode::Raw {
            return self.prepare_raw(core);
        }
        let pending = core.state.collector.pending_source();
        if pending.is_empty()
            || pending.contains('|')
            || pending.starts_with([' ', '\t', '>'])
            || pending.starts_with("~~~")
            || pending.starts_with("```")
            || !core.holdback_scanner.allows_prose_preview()
            || core.render.pending_math_start.is_some()
        {
            return self.hold_or_drain(core);
        }
        let source = core.state.collector.source();
        let committed = core.state.collector.committed_source().len();
        let start = if core.render.has_reference_link_definition {
            0
        } else {
            self.unresolved_reference
                .unwrap_or(self.scanned.min(core.render.completed_source_len))
        }
        .min(committed);
        let mut unresolved = None;
        let mut missing = |link: pulldown_cmark::BrokenLink<'_>| {
            unresolved.get_or_insert(start + link.span.start);
            None
        };
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        let mut inline = None;
        let mut code = false;
        for (event, range) in
            Parser::new_with_broken_link_callback(&source[start..], options, Some(&mut missing))
                .into_offset_iter()
        {
            match event {
                Event::Start(Tag::CodeBlock(_)) => code = true,
                Event::End(TagEnd::CodeBlock) => code = false,
                Event::Text(_) if !code => {
                    if let Some(offset) = open_inline(&source[start..], range) {
                        inline.get_or_insert(start + offset);
                    }
                }
                _ => {}
            }
        }
        self.unresolved_reference = unresolved;
        self.scanned = core.render.completed_source_len;
        let bound = inline
            .into_iter()
            .chain(unresolved)
            .min()
            .unwrap_or(source.len());
        if bound <= committed {
            return self.hold_or_drain(core);
        }
        let safe = source[committed..bound]
            .char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map_or(committed, |(offset, ch)| committed + offset + ch.len_utf8());
        if safe <= committed {
            return self.hold_or_drain(core);
        }
        let base_source = core.render.stable_source_len.min(committed);
        let base_lines = core.render.stable_rendered_len;
        let mut rendered = render_markdown_agent_with_list_spacing(
            &source[base_source..safe],
            core.width,
            Some(&core.cwd),
            core.inline_visualization_context.as_ref(),
            core.render.list_spacing,
        );
        let offset = base_lines + usize::from(base_lines > 0 && !rendered.is_empty());
        let tail = rendered.pop().into_iter().collect::<Vec<_>>();
        let target = offset + rendered.len();
        let emitted = core.emitted_stable_len;
        if target < emitted {
            return (Vec::new(), None);
        }
        let mut rows = Vec::new();
        if emitted < offset {
            rows.extend_from_slice(
                &core.render.lines[emitted..offset.min(core.render.lines.len())],
            );
        }
        rows.extend(rendered.drain(emitted.saturating_sub(offset)..));
        core.state.clear_queue();
        core.enqueued_stable_len = target;
        core.emitted_stable_len = target;
        self.partial = true;
        (rows, Some(tail))
    }

    pub(super) fn drain(&mut self, core: &mut StreamCore) -> Vec<HyperlinkLine> {
        if core.render_mode == HistoryRenderMode::Raw {
            return core.tick_batch(usize::MAX);
        }
        if core.emitted_stable_len > core.render.lines.len() {
            return Vec::new();
        }
        let source = core.state.collector.committed_source();
        let start = if core.render.has_reference_link_definition {
            0
        } else {
            self.unresolved_reference
                .unwrap_or(self.scanned.min(core.render.completed_source_len))
        }
        .min(source.len());
        let input = &source[start..];
        let mut unresolved = None;
        let mut missing = |link: pulldown_cmark::BrokenLink<'_>| {
            unresolved.get_or_insert(start + link.span.start);
            None
        };
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        let mut code = false;
        let mut ambiguous = None;
        for (event, range) in
            Parser::new_with_broken_link_callback(input, options, Some(&mut missing))
                .into_offset_iter()
        {
            match event {
                Event::Start(Tag::CodeBlock(_)) => code = true,
                Event::End(TagEnd::CodeBlock) => code = false,
                Event::Text(_)
                    if !code && start + range.start >= core.render.completed_source_len =>
                {
                    if let Some(offset) = open_inline(input, range) {
                        ambiguous.get_or_insert(start + offset);
                    }
                }
                _ => {}
            }
        }
        self.unresolved_reference = unresolved;
        self.scanned = core.render.completed_source_len;
        let boundary = ambiguous.into_iter().chain(unresolved).min();
        if let Some(boundary) = boundary {
            let line_start = source[..boundary]
                .rfind('\n')
                .map_or(0, |offset| offset + 1);
            let (base_source, base_lines) = if line_start >= core.render.stable_source_len {
                (
                    core.render.stable_source_len,
                    core.render.stable_rendered_len,
                )
            } else {
                (0, 0)
            };
            let prefix = render_markdown_agent_with_list_spacing(
                &source[base_source..line_start],
                core.width,
                Some(&core.cwd),
                core.inline_visualization_context.as_ref(),
                core.render.list_spacing,
            );
            let safe =
                (base_lines + usize::from(base_lines > 0 && !prefix.is_empty()) + prefix.len())
                    .min(core.enqueued_stable_len)
                    .max(core.emitted_stable_len);
            core.state.clear_queue();
            core.state
                .enqueue(core.render.lines[core.emitted_stable_len..safe].to_vec());
            core.enqueued_stable_len = safe;
        }
        core.tick_batch(usize::MAX)
    }
}

// Only literal parser text can contain an unmatched opener; code, links and paired
// delimiters have already been classified by pulldown-cmark. Flanking excludes
// literal multiplication, intraword underscores and escaped delimiters.
fn open_inline(source: &str, range: std::ops::Range<usize>) -> Option<usize> {
    for (relative, marker) in source[range.clone()].char_indices() {
        if !matches!(marker, '*' | '_' | '~' | '`' | '[' | '<') {
            continue;
        }
        let offset = range.start + relative;
        if source[..offset]
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\\')
            .count()
            % 2
            != 0
        {
            continue;
        }
        if marker == '[' || marker == '`' || marker == '<' {
            return Some(offset);
        }
        if marker == '~' && !source[offset..].starts_with("~~") {
            continue;
        }
        let run = source[offset..]
            .chars()
            .take_while(|ch| *ch == marker)
            .count();
        let before = source[..offset].chars().next_back();
        let after = source[offset + run..].chars().next();
        let punctuation = |ch: char| !ch.is_alphanumeric() && !ch.is_whitespace();
        let left = after.is_some_and(|ch| !ch.is_whitespace())
            && (after.is_some_and(|ch| !punctuation(ch))
                || before.is_none_or(|ch| ch.is_whitespace() || punctuation(ch)));
        let right = before.is_some_and(|ch| !ch.is_whitespace())
            && (before.is_some_and(|ch| !punctuation(ch))
                || after.is_none_or(|ch| ch.is_whitespace() || punctuation(ch)));
        if left && (marker != '_' || !right || before.is_some_and(punctuation)) {
            return Some(offset);
        }
    }
    None
}
