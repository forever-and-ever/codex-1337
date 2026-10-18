//! Worker-local Markdown source and parser state; the UI receives only new stable rows.
//! Setext underlines and late table pipes are the two deliberate baseline exceptions:
//! they may restyle a previously plain paragraph when canonical rows replace the stream.

//! The live UI prepares source on a pinned lane of the bounded transcript pool and publishes
//! immutable stable rows. Open inline syntax, unresolved references, tables and transformable
//! fences stay on the worker. Two deliberate baseline exceptions can revise already streamed
//! plain prose: a directly following setext underline (===/---), and a late pipe that makes
//! the current line table-eligible. A pipe freezes that line until its structure is known.
//! Canonical replacement resolves both, rather than delaying every paragraph until a blank line.
//! Safe complete words on an unterminated prose line wrap with the final renderer's `FirstFit`:
//! completed rows never roll; only the last row grows. Source parsing and unresolved-block
//! rendering run on the worker, never in the delta handler or paint path.
//!

use super::HistoryCell;
use super::StablePrefixLenCache;
use super::StreamCore;
use super::history_cell;
use crate::history_cell::HistoryRenderMode;
use crate::markdown::render_markdown_agent_with_list_spacing;
use crate::markdown_render::ListSpacing;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::transcript_view::preparation::Lane;
use crate::transcript_view::preparation::Pending;
use crate::tui::FrameRequester;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
pub(crate) type Rewrap =
    Arc<dyn Fn(Vec<HyperlinkLine>, Option<usize>, HistoryRenderMode) + Send + Sync>;
use std::time::Duration;
use std::time::Instant;

pub(super) enum Rows {
    Inline(Vec<HyperlinkLine>),
    Shared(history_cell::SharedRows),
}
impl Rows {
    pub(super) fn is_empty(&self) -> bool {
        match self {
            Self::Inline(rows) => rows.is_empty(),
            Self::Shared(rows) => rows.is_empty(),
        }
    }
}
impl From<Vec<HyperlinkLine>> for Rows {
    fn from(rows: Vec<HyperlinkLine>) -> Self {
        Self::Inline(rows)
    }
}
#[cfg(test)]
impl IntoIterator for Rows {
    type Item = HyperlinkLine;
    type IntoIter = std::vec::IntoIter<HyperlinkLine>;
    fn into_iter(self) -> Self::IntoIter {
        match self {
            Self::Inline(rows) => rows,
            Self::Shared(rows) => rows.materialize(),
        }
        .into_iter()
    }
}

pub(super) enum Core {
    Inline(Box<StreamCore>),
    Background(Box<Background>),
}

enum Command {
    Delta(String),
    Width(Option<usize>),
    Mode(HistoryRenderMode),
    Clear,
}
impl Command {
    fn apply(self, core: &mut StreamCore) {
        match self {
            Self::Delta(delta) => {
                let _ = core.push_delta(&delta);
            }
            Self::Width(width) => core.set_width(width),
            Self::Mode(mode) => core.set_render_mode(mode),
            Self::Clear => {
                core.state.clear_queue();
                core.enqueued_stable_len = core.emitted_stable_len;
            }
        }
    }
}

thread_local! { static STREAMS: RefCell<HashMap<u64, (StreamCore, super::immutable_rows::Publication)>> = RefCell::default(); }
struct Ready {
    rows: Arc<[HyperlinkLine]>,
    tail: Option<Vec<HyperlinkLine>>,
    rewrapped: Option<usize>,
    width: Option<usize>,
    mode: HistoryRenderMode,
}

enum Work {
    Idle,
    Running(Pending<Ready>),
    Closed,
}

pub(crate) struct Background {
    work: Work,
    commands: Vec<Command>,
    lines: VecDeque<history_cell::RowChunk>,
    queued: usize,
    tail: Vec<HyperlinkLine>,
    lane: Lane,
    rewrap: Option<Rewrap>,
    id: u64,
    width: Option<usize>,
    mode: HistoryRenderMode,
    emitted: usize,
    rewrapped: bool,
    oldest: Option<Instant>,
}

impl Background {
    fn new(core: Box<StreamCore>, frames: FrameRequester) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let lane = Lane::new(frames);
        let StreamCore {
            width,
            cwd,
            render_mode: mode,
            inline_visualization_context: context,
            render,
            ..
        } = *core;
        let spacing = render.list_spacing;
        let mut init = Pending::on_lane(&lane, move || {
            let mut core = StreamCore::new(width, &cwd, mode, context);
            core.render.list_spacing = spacing;
            STREAMS.with(|streams| {
                let _ = streams
                    .borrow_mut()
                    .insert(id, (core, super::immutable_rows::Publication::default()));
            });
            Ready {
                rewrapped: None,
                rows: Arc::from([]),
                tail: Some(Vec::new()),
                width,
                mode,
            }
        });
        init.submit();
        Self {
            rewrapped: false,
            rewrap: None,
            width,
            mode,
            id,
            lane,
            work: Work::Running(init),
            commands: Vec::new(),
            lines: VecDeque::new(),
            queued: 0,
            tail: Vec::new(),
            emitted: 0,
            oldest: None,
        }
    }
    #[expect(
        clippy::expect_used,
        reason = "The pinned lane initializes this stream before executing any later command."
    )]
    fn poll(&mut self) {
        if let Work::Running(task) = &mut self.work
            && let Some(lines) = task.poll()
        {
            if lines.width == self.width && lines.mode == self.mode {
                if let Some(count) = lines.rewrapped {
                    self.rewrapped = count > 0;
                    self.emitted = count;
                }
                self.queued += lines.rows.len();
                if !lines.rows.is_empty() {
                    self.lines.push_back(history_cell::RowChunk {
                        range: 0..lines.rows.len(),
                        rows: lines.rows,
                    });
                }
                if let Some(tail) = lines.tail {
                    self.tail = tail;
                }
            } else {
                self.lane.spawn(move || drop(lines));
            }
            self.work = Work::Idle;
        }
        if self.commands.is_empty() || !matches!(self.work, Work::Idle) {
            return;
        }
        let commands = std::mem::take(&mut self.commands);
        let id = self.id;
        let publisher = self.rewrap.clone();
        let mut task = Pending::on_lane(&self.lane, move || {
            STREAMS.with(|streams| {
                let mut streams = streams.borrow_mut();
                let (core, publication) = streams.get_mut(&id).expect("initialized stream");
                let mut rewrap = commands
                    .iter()
                    .any(|command| matches!(command, Command::Width(_) | Command::Mode(_)));
                for command in commands {
                    if let Command::Delta(delta) = &command {
                        publication.observe(core, delta);
                    }
                    command.apply(core);
                }
                rewrap |= publication.needs_rewrap(core);
                if rewrap {
                    publication.reset_partial();
                    core.emitted_stable_len = 0;
                    core.enqueued_stable_len = 0;
                    core.state.clear_queue();
                    let _ = core.sync_stable_queue();
                }
                let (rows, tail) = publication.prepare(core);
                let count = rows.len();
                let rewrapped = (rewrap && publisher.is_some()).then_some(count);
                let rows = if rewrap && let Some(publish) = publisher {
                    publish(rows, core.width, core.render_mode);
                    Vec::new()
                } else {
                    rows
                };
                Ready {
                    tail,
                    rewrapped,
                    rows: rows.into(),
                    width: core.width,
                    mode: core.render_mode,
                }
            })
        });
        task.submit();
        self.work = Work::Running(task);
    }
    fn push(&mut self, command: Command) {
        let _ = self.oldest.get_or_insert_with(Instant::now);
        self.commands.push(command);
        self.poll();
    }
    #[expect(
        clippy::expect_used,
        reason = "A chunk selected from the exclusively borrowed queue remains at its front until removed."
    )]
    fn drain(&mut self, max: usize) -> history_cell::SharedRows {
        self.poll();
        let mut rows = history_cell::SharedRows::default();
        while rows.len() < max {
            let Some(chunk) = self.lines.front_mut() else {
                break;
            };
            let count = (max - rows.len()).min(chunk.range.len());
            if count == chunk.range.len() {
                rows.push(self.lines.pop_front().expect("front chunk"));
            } else {
                rows.push(history_cell::RowChunk {
                    rows: Arc::clone(&chunk.rows),
                    range: chunk.range.start..chunk.range.start + count,
                });
                chunk.range.start += count;
            }
            self.queued -= count;
        }
        self.emitted += rows.len();
        if self.is_idle() {
            self.oldest = None;
        }
        rows
    }
    fn discard_lines(&mut self) {
        let lines = std::mem::take(&mut self.lines);
        self.queued = 0;
        self.lane.spawn(move || drop(lines));
    }
    fn is_idle(&self) -> bool {
        self.lines.is_empty() && self.commands.is_empty() && matches!(self.work, Work::Idle)
    }
    #[expect(
        clippy::expect_used,
        reason = "Consuming the stream finalizes its initialized pinned-lane state exactly once."
    )]
    pub(crate) fn finish(mut self, finish: impl FnOnce(String) + Send + 'static) {
        let work = std::mem::replace(&mut self.work, Work::Closed);
        let commands = std::mem::take(&mut self.commands);
        let lines = std::mem::take(&mut self.lines);
        let lane = self.lane.clone();
        let id = self.id;
        drop(tokio::spawn(async move {
            let ready = match work {
                Work::Running(task) => Some(task.wait().await),
                _ => None,
            };
            lane.spawn(move || {
                drop(ready);
                drop(lines);
                STREAMS.with(|streams| {
                    let (mut core, _) = streams
                        .borrow_mut()
                        .remove(&id)
                        .expect("a stream finalizes once");
                    for command in commands {
                        command.apply(&mut core);
                    }
                    finish(core.state.collector.finalize_and_take_source());
                });
            });
        }));
    }
}
impl Drop for Background {
    fn drop(&mut self) {
        let work = std::mem::replace(&mut self.work, Work::Closed);
        if matches!(work, Work::Closed) {
            return;
        }
        let id = self.id;
        let lane = self.lane.clone();
        let lines = std::mem::take(&mut self.lines);
        drop(tokio::spawn(async move {
            let ready = match work {
                Work::Running(task) => Some(task.wait().await),
                _ => None,
            };
            lane.spawn(move || {
                drop(ready);
                drop(lines);
                STREAMS.with(|streams| {
                    let _ = streams.borrow_mut().remove(&id);
                });
            });
        }));
    }
}

impl Core {
    #[cfg(test)]
    pub(super) async fn settle(&mut self) {
        if let Self::Background(background) = self {
            loop {
                background.poll();
                let Work::Running(_) = &background.work else {
                    break;
                };
                let Work::Running(task) = std::mem::replace(&mut background.work, Work::Closed)
                else {
                    unreachable!()
                };
                let ready = task.wait().await;
                if ready.width == background.width && ready.mode == background.mode {
                    if let Some(count) = ready.rewrapped {
                        background.rewrapped = count > 0;
                        background.emitted = count;
                    }
                    background.queued += ready.rows.len();
                    if !ready.rows.is_empty() {
                        background.lines.push_back(history_cell::RowChunk {
                            range: 0..ready.rows.len(),
                            rows: ready.rows,
                        });
                    }
                    if let Some(tail) = ready.tail {
                        background.tail = tail;
                    }
                }
                background.work = Work::Idle;
            }
        }
    }

    pub(super) fn has_rewrap_rows(&self) -> bool {
        matches!(self, Self::Background(background) if background.rewrapped)
    }

    pub(super) fn wrap_rows(
        &self,
        rows: history_cell::SharedRows,
        projection: history_cell::RowProjection,
        item_id: Option<String>,
        reserved: usize,
    ) -> Box<dyn HistoryCell> {
        let Self::Background(core) = self else {
            unreachable!("shared rows belong to prepared streams");
        };
        Box::new(history_cell::StreamRowsCell {
            mode: core.mode,
            width: core.width,
            reserved,
            item_id,
            content: history_cell::StreamRowContent::Shared { rows, projection },
        })
    }

    pub(super) fn inline(&self) -> Option<&StreamCore> {
        match self {
            Self::Inline(core) => Some(core),
            Self::Background(_) => None,
        }
    }

    pub(super) fn with_background(self, frames: FrameRequester) -> Self {
        match self {
            Self::Inline(core) => Self::Background(Box::new(Background::new(core, frames))),
            Self::Background(_) => self,
        }
    }

    #[expect(
        clippy::panic,
        reason = "Stream spacing is construction-only configuration; prepared streams cannot be reconstructed in place."
    )]
    pub(super) fn set_list_spacing(&mut self, spacing: ListSpacing) {
        match self {
            Self::Inline(core) => core.render.list_spacing = spacing,
            Self::Background(_) => panic!("configure stream spacing before enabling preparation"),
        }
    }

    pub(super) fn push_delta(&mut self, delta: &str) -> bool {
        match self {
            Self::Inline(core) => core.push_delta(delta),
            Self::Background(core) => {
                if delta.is_empty() {
                    return false;
                }
                core.push(Command::Delta(delta.to_owned()));
                true
            }
        }
    }

    pub(super) fn tick(&mut self) -> Rows {
        match self {
            Self::Inline(core) => Rows::Inline(core.tick()),
            Self::Background(core) => Rows::Shared(core.drain(1)),
        }
    }

    pub(super) fn tick_batch(&mut self, max: usize) -> Rows {
        match self {
            Self::Inline(core) => Rows::Inline(core.tick_batch(max)),
            Self::Background(core) => Rows::Shared(core.drain(max)),
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        match self {
            Self::Inline(core) => core.is_idle(),
            Self::Background(core) => core.is_idle(),
        }
    }

    pub(super) fn queued_lines(&self) -> usize {
        match self {
            Self::Inline(core) => core.queued_lines(),
            Self::Background(core) => core.queued.max(usize::from(!core.is_idle())),
        }
    }

    pub(super) fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        match self {
            Self::Inline(core) => core.oldest_queued_age(now),
            Self::Background(core) => core
                .oldest
                .map(|oldest| now.saturating_duration_since(oldest)),
        }
    }

    pub(super) fn current_tail_lines(&self) -> Vec<HyperlinkLine> {
        match self {
            Self::Inline(core) => core.current_tail_lines(),
            Self::Background(core) => core.tail.clone(),
        }
    }

    pub(super) fn has_tail(&self) -> bool {
        match self {
            Self::Inline(core) => core.has_tail(),
            Self::Background(core) => !core.tail.is_empty(),
        }
    }

    pub(super) fn enqueued_lines(&self) -> usize {
        match self {
            Self::Inline(core) => core.enqueued_stable_len,
            Self::Background(core) => core.emitted + core.queued,
        }
    }

    pub(super) fn set_width(&mut self, width: Option<usize>) {
        match self {
            Self::Inline(core) => core.set_width(width),
            Self::Background(core) if core.width != width => {
                core.width = width;
                core.discard_lines();
                core.tail.clear();
                core.push(Command::Width(width));
            }
            Self::Background(_) => {}
        }
    }

    pub(super) fn set_render_mode(&mut self, mode: HistoryRenderMode) {
        match self {
            Self::Inline(core) => core.set_render_mode(mode),
            Self::Background(core) if core.mode != mode => {
                core.mode = mode;
                core.discard_lines();
                core.tail.clear();
                core.push(Command::Mode(mode));
            }
            Self::Background(_) => {}
        }
    }

    pub(super) fn clear_queue(&mut self) {
        match self {
            Self::Inline(core) => {
                core.state.clear_queue();
                core.enqueued_stable_len = core.emitted_stable_len;
            }
            Self::Background(core) => {
                core.discard_lines();
                core.push(Command::Clear);
            }
        }
    }

    #[expect(
        clippy::panic,
        reason = "Prepared streams must transfer completion through their keyed finalization owner, never the synchronous inline API."
    )]
    pub(super) fn finalize_remaining(&mut self) -> (Vec<HyperlinkLine>, String) {
        match self {
            Self::Inline(core) => core.finalize_remaining(),
            Self::Background(_) => {
                panic!("background streams finalize through their keyed completion")
            }
        }
    }

    #[expect(
        clippy::panic,
        reason = "Prepared streams transfer their parser on finalization and cannot be reset through the synchronous inline API."
    )]
    pub(super) fn reset(&mut self) {
        match self {
            Self::Inline(core) => core.reset(),
            Self::Background(_) => panic!("background streams are consumed on finalization"),
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
 pub(crate) fn with_rewraps(mut self, publish: Rewrap) -> Self {
                let Core::Background(background) = &mut self.core else { panic!("enable preparation before rewraps"); };
                background.rewrap = Some(publish);
                self
            }
            pub(crate) fn is_background(&self) -> bool { matches!(self.core, Core::Background(_)) }
            pub(crate) fn with_background(mut self, frames: FrameRequester) -> Self {
                self.core = self.core.with_background(frames);
                self
            }

            pub(crate) fn finish_background(
                self,
                finish: impl FnOnce(String) + Send + 'static,
            ) -> Result<(), Self> {
                match self.core {
                    Core::Background(background) => {
                        background.finish(finish);
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
    pub(super) fn emit_rows(&mut self, lines: impl Into<Rows>) -> Option<Box<dyn HistoryCell>> {
        let lines = lines.into();
        if lines.is_empty() {
            return None;
        }
        self.header_emitted |= self.core.has_rewrap_rows();
        let first = !self.header_emitted;
        self.header_emitted = true;
        Some(match lines {
            Rows::Inline(lines) => {
                let mut cell = history_cell::AgentMessageCell::new_hyperlink_lines(lines, first);
                cell.timestamp = self.timestamp;
                cell.item_id.clone_from(&self.item_id);
                Box::new(cell)
            }
            Rows::Shared(rows) => self.core.wrap_rows(
                rows,
                history_cell::RowProjection::Agent {
                    first,
                    timestamp: self.timestamp,
                },
                self.item_id.clone(),
                history_cell::agent_prefix(true).width(),
            ),
        })
    }
}

impl super::PlanStreamController {
    pub(super) fn emit_rows(
        &mut self,
        lines: impl Into<Rows>,
        include_bottom_padding: bool,
    ) -> Option<Box<dyn HistoryCell>> {
        let lines = lines.into();
        if lines.is_empty() && !include_bottom_padding {
            return None;
        }
        self.header_emitted |= self.core.has_rewrap_rows();
        self.top_padding_emitted |= self.core.has_rewrap_rows();
        let first = !self.header_emitted;
        let top = !self.top_padding_emitted;
        let cell = match lines {
            Rows::Inline(lines) => {
                let out = self.render_display_lines(lines, include_bottom_padding);
                let mut cell = history_cell::new_proposed_plan_stream(out, !first);
                cell.item_id.clone_from(&self.item_id);
                Box::new(cell) as Box<dyn HistoryCell>
            }
            Rows::Shared(rows) => self.core.wrap_rows(
                rows,
                history_cell::RowProjection::Plan {
                    first,
                    top,
                    bottom: include_bottom_padding,
                },
                self.item_id.clone(),
                4,
            ),
        };
        self.header_emitted = true;
        self.top_padding_emitted = true;
        Some(cell)
    }

    pub(crate) fn emit_prepared(
        &mut self,
        lines: Vec<HyperlinkLine>,
    ) -> Option<Box<dyn HistoryCell>> {
        self.emit(lines, false)
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
