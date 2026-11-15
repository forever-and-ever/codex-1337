//! Fullscreen responses retain source only; parsing starts once the item is complete.
//! The inline core remains for the upstream renderer, not for live fullscreen output.
//! TELOS(stable-feed-publication): no partial response rows reach the fullscreen feed.
//! TELOS(fast-ui-thread): completion preparation runs on the worker pool.

use super::HistoryCell;
use super::StablePrefixLenCache;
use super::StreamCore;
use super::history_cell;
use crate::history_cell::HistoryRenderMode;
use crate::markdown::render_markdown_agent_with_list_spacing;
use crate::markdown_render::ListSpacing;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::transcript_view::preparation::Lane;
use crate::tui::FrameRequester;
use std::time::Duration;
use std::time::Instant;

pub(super) enum Core {
    Inline(Box<StreamCore>),
    Background(Background),
}

pub(super) struct Background {
    source: String,
    lane: Lane,
}

impl Core {
    pub(super) fn source_snapshot(&self) -> String {
        match self {
            Self::Inline(core) => format!(
                "{}{}",
                core.state.collector.committed_source(),
                core.state.collector.pending_source()
            ),
            Self::Background(core) => core.source.clone(),
        }
    }

    pub(super) fn inline(&self) -> Option<&StreamCore> {
        match self {
            Self::Inline(core) => Some(core),
            Self::Background(_) => None,
        }
    }

    pub(super) fn with_background(self, frames: FrameRequester) -> Self {
        match self {
            Self::Inline(_) => Self::Background(Background {
                source: String::new(),
                lane: Lane::new(frames),
            }),
            Self::Background(_) => self,
        }
    }

    pub(super) fn set_list_spacing(&mut self, spacing: ListSpacing) {
        if let Self::Inline(core) = self {
            core.render.list_spacing = spacing;
        }
    }

    pub(super) fn push_delta(&mut self, delta: &str) -> bool {
        match self {
            Self::Inline(core) => core.push_delta(delta),
            Self::Background(core) => {
                core.source.push_str(delta);
                // This reports newly paintable rows, not receipt of model tokens.
                false
            }
        }
    }

    pub(super) fn tick(&mut self) -> Vec<HyperlinkLine> {
        match self {
            Self::Inline(core) => core.tick(),
            Self::Background(_) => Vec::new(),
        }
    }

    pub(super) fn tick_batch(&mut self, max: usize) -> Vec<HyperlinkLine> {
        match self {
            Self::Inline(core) => core.tick_batch(max),
            Self::Background(_) => Vec::new(),
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        match self {
            Self::Inline(core) => core.is_idle(),
            Self::Background(_) => true,
        }
    }

    pub(super) fn queued_lines(&self) -> usize {
        match self {
            Self::Inline(core) => core.queued_lines(),
            Self::Background(_) => 0,
        }
    }

    pub(super) fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        match self {
            Self::Inline(core) => core.oldest_queued_age(now),
            Self::Background(_) => None,
        }
    }

    pub(super) fn current_tail_lines(&self) -> Vec<HyperlinkLine> {
        match self {
            Self::Inline(core) => core.current_tail_lines(),
            Self::Background(_) => Vec::new(),
        }
    }

    pub(super) fn has_tail(&self) -> bool {
        match self {
            Self::Inline(core) => core.has_tail(),
            Self::Background(_) => false,
        }
    }

    pub(super) fn enqueued_lines(&self) -> usize {
        match self {
            Self::Inline(core) => core.enqueued_stable_len,
            Self::Background(_) => 0,
        }
    }

    pub(super) fn set_width(&mut self, width: Option<usize>) {
        if let Self::Inline(core) = self {
            core.set_width(width);
        }
    }

    pub(super) fn set_render_mode(&mut self, mode: HistoryRenderMode) {
        if let Self::Inline(core) = self {
            core.set_render_mode(mode);
        }
    }

    pub(super) fn clear_queue(&mut self) {
        if let Self::Inline(core) = self {
            core.state.clear_queue();
            core.enqueued_stable_len = core.emitted_stable_len;
        }
    }

    #[expect(
        clippy::panic,
        reason = "Fullscreen responses transfer completion through their keyed owner, never the synchronous inline API."
    )]
    pub(super) fn finalize_remaining(&mut self) -> (Vec<HyperlinkLine>, String) {
        match self {
            Self::Inline(core) => core.finalize_remaining(),
            Self::Background(_) => {
                panic!("buffered responses finalize through their keyed completion")
            }
        }
    }

    #[expect(
        clippy::panic,
        reason = "Fullscreen responses are consumed on finalization, not reset through the inline API."
    )]
    pub(super) fn reset(&mut self) {
        match self {
            Self::Inline(core) => core.reset(),
            Self::Background(_) => panic!("buffered responses are consumed on finalization"),
        }
    }
}

impl From<StreamCore> for Core {
    fn from(core: StreamCore) -> Self {
        Self::Inline(Box::new(core))
    }
}

macro_rules! controllers {
    ($($controller:ident),+ $(,)?) => {$(
        impl super::$controller {
            pub(crate) fn is_background(&self) -> bool {
                matches!(self.core, Core::Background(_))
            }

            pub(crate) fn with_background(mut self, frames: FrameRequester) -> Self {
                self.core = self.core.with_background(frames);
                self
            }

            pub(crate) fn finish_background(
                self,
                finish: impl FnOnce(String) + Send + 'static,
            ) -> Result<(), Self> {
                match self.core {
                    Core::Background(Background { source, lane }) => {
                        lane.spawn(move || finish(source));
                        Ok(())
                    }
                    core => Err(Self { core, ..self }),
                }
            }
        }
    )+};
}

controllers!(StreamController, PlanStreamController);

impl super::StreamController {
    pub(super) fn emit_rows(&mut self, lines: Vec<HyperlinkLine>) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() {
            return None;
        }
        let mut cell =
            history_cell::AgentMessageCell::new_hyperlink_lines(lines, !self.header_emitted);
        self.header_emitted = true;
        cell.timestamp = self.timestamp;
        cell.item_id.clone_from(&self.item_id);
        Some(Box::new(cell))
    }
}

impl super::PlanStreamController {
    pub(super) fn emit_rows(
        &mut self,
        lines: Vec<HyperlinkLine>,
        include_bottom_padding: bool,
    ) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() && !include_bottom_padding {
            return None;
        }
        let first = !self.header_emitted;
        let out = self.render_display_lines(lines, include_bottom_padding);
        let mut cell = history_cell::new_proposed_plan_stream(out, !first);
        cell.item_id.clone_from(&self.item_id);
        self.header_emitted = true;
        self.top_padding_emitted = true;
        Some(Box::new(cell))
    }
}

impl StreamCore {
    pub(super) fn prepared_prefix_len(&mut self, source_start: usize) -> usize {
        if let Some(cache) = &self.stable_prefix_len_cache
            && cache.source_start == source_start
            && cache.width == self.width
        {
            tracing::trace!(
                source_start,
                width = ?self.width,
                stable_prefix_len = cache.stable_prefix_len,
                "table holdback stable-prefix cache hit",
            );
            return cache.stable_prefix_len;
        }

        let render_start = Instant::now();
        let source = self.state.collector.committed_source();
        let source_start = source_start.min(source.len());
        let (base_source, base_lines) = if source_start >= self.render.stable_source_len {
            (
                self.render.stable_source_len,
                self.render.stable_rendered_len,
            )
        } else {
            (0, 0)
        };
        let stable_prefix_render = render_markdown_agent_with_list_spacing(
            &source[base_source..source_start],
            self.width,
            Some(self.cwd.as_path()),
            self.inline_visualization_context.as_ref(),
            self.render.list_spacing,
        );
        let stable_prefix_len = base_lines
            + usize::from(base_lines > 0 && !stable_prefix_render.is_empty())
            + stable_prefix_render.len();
        tracing::trace!(
            source_start,
            width = ?self.width,
            stable_prefix_len,
            elapsed_us = render_start.elapsed().as_micros(),
            "table holdback stable-prefix render",
        );
        self.stable_prefix_len_cache = Some(StablePrefixLenCache {
            source_start,
            width: self.width,
            stable_prefix_len,
        });
        stable_prefix_len
    }
}
