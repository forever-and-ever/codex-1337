//! The TUI's own shimmer: one per process, fed by the displayed thread and drawn by its live rows.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;

use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use rand::Rng as _;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Span;

use super::ROSTER;
use super::engine::Engine;
use super::engine::color;
use super::stage::Palette;
use super::stage::Rgb;
use super::stage::Spell;
use super::stage::TICK;
use crate::motion::MotionMode;
use crate::terminal_palette::StdoutColorLevel;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::default_fg;
use crate::terminal_palette::effective_stdout_color_level;
use crate::tui::FrameRequester;
use crate::tui::PresentationMode;

/// A gap in drawing longer than this, as between turns, pauses the shimmer's clock.
const STALE: Duration = Duration::from_secs(/*secs*/ 1);

static LIVE: LazyLock<Mutex<Live>> = LazyLock::new(Mutex::default);

#[derive(Default)]
struct Live {
    feed: Feed,
    frames: Option<FrameRequester>,
    act: Act,
    /// The backdrop of the status row dressed last, which `shade_status` lays under the rendered row.
    backdrop: Vec<Rgb>,
}

#[derive(Default)]
enum Act {
    /// Nothing is engaged; the classic sweep runs.
    #[default]
    Classic,
    /// Engaged but not yet drawn, so the terminal's colors are still unknown.
    Engaged(usize),
    /// Drawing; `stepped` is when its clock last ticked.
    Running {
        engine: Box<Engine>,
        stepped: Instant,
    },
}

impl Live {
    fn draws_motion(&self) -> bool {
        self.frames
            .as_ref()
            .is_some_and(|frames| frames.mode() == PresentationMode::Normal)
    }

    /// The running engine, stepped up to now.
    fn current(&mut self, palette: Palette) -> Option<&mut Engine> {
        if !self.draws_motion() {
            return None;
        }
        let now = Instant::now();
        let tick = Duration::from_secs_f64(TICK);
        let restart = now.checked_sub(tick).unwrap_or(now);
        if let Act::Engaged(recipe) = self.act {
            self.act = Act::Running {
                engine: Box::new(Engine::new(recipe, palette)),
                stepped: restart,
            };
            self.feed.pending = 0;
        }
        let Act::Running { engine, stepped } = &mut self.act else {
            return None;
        };
        if now.saturating_duration_since(*stepped) > STALE {
            *stepped = restart;
            self.feed.pending = 0;
        }
        while now.saturating_duration_since(*stepped) >= tick {
            *stepped += tick;
            engine.step(std::mem::take(&mut self.feed.pending), self.feed.spell());
        }
        Some(engine)
    }
}

fn live() -> MutexGuard<'static, Live> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The terminal's colors, when 24-bit color lets a shimmer use them.
pub(super) fn palette() -> Option<Palette> {
    (effective_stdout_color_level() == StdoutColorLevel::TrueColor).then_some(())?;
    Some(Palette {
        fg: default_fg()?.into(),
        bg: default_bg()?.into(),
    })
}

/// Read the terminal's canonical attention state instead of keeping another focus flag.
pub(crate) fn follow_presentation(frames: FrameRequester) {
    live().frames = Some(frames);
}

/// Engages a random stock shimmer. Until something does, as in tests, the classic sweep runs.
pub(crate) fn engage_random() {
    engage(rand::rng().random_range(0..ROSTER.len()));
}

pub(super) fn engage(recipe: usize) {
    live().act = Act::Engaged(recipe);
}

/// The roster entry engaged, if any.
pub(super) fn engaged() -> Option<usize> {
    match &live().act {
        Act::Classic => None,
        Act::Engaged(recipe) => Some(*recipe),
        Act::Running { engine, .. } => Some(engine.recipe),
    }
}

/// Feeds the shimmer from a live notification of the displayed thread.
pub(crate) fn observe(notification: &ServerNotification) {
    live().feed.observe(notification);
}

/// Replaces the status row's lead in `spans`, the classic indicator and header, with the engaged
/// shimmer's. Leaves `spans` alone when nothing is engaged, nothing moves, or colors are unknown.
pub(crate) fn dress_status(
    spans: &mut Vec<Span<'static>>,
    header: &str,
    elapsed: Duration,
    mark: MotionMode,
    letters: MotionMode,
) {
    let mut live = live();
    live.backdrop.clear();
    let (mark, letters) = (
        mark == MotionMode::Animated,
        letters == MotionMode::Animated,
    );
    if !(mark || letters) {
        return;
    }
    let Some(palette) = palette() else {
        return;
    };
    let Some(engine) = live.current(palette) else {
        return;
    };
    let (lead, backdrop) = engine.status(header, elapsed.as_secs(), palette, mark, letters);
    *spans = lead;
    live.backdrop = backdrop;
}

/// Lays the dressed status row's backdrop under the row rendered at the top of `area`.
pub(crate) fn shade_status(area: Rect, buf: &mut Buffer) {
    let live = live();
    if !live.draws_motion() {
        return;
    }
    for (x, bg) in (area.x..area.right()).zip(&live.backdrop) {
        buf[(x, area.y)].set_bg(color(*bg));
    }
}

pub(crate) fn tool_mark() -> Option<Span<'static>> {
    let mut live = live();
    if !live.draws_motion() {
        return (!matches!(live.act, Act::Classic)).then(|| "•".dim());
    }
    let palette = palette()?;
    live.current(palette).map(|engine| engine.tool(palette))
}

/// What the displayed thread streams and runs, distilled for the shimmer.
#[derive(Default)]
struct Feed {
    /// The thread that fed this last; another thread's notification forgets the tools in flight.
    thread: String,
    /// Tool items in flight: while any is, the agent waits.
    tools: HashSet<String>,
    /// Streamed bytes not yet counted as a token.
    carry: usize,
    /// Tokens since the engine last stepped.
    pending: u32,
}

impl Feed {
    fn observe(&mut self, notification: &ServerNotification) {
        let (thread, text) = match notification {
            ServerNotification::TurnStarted(turn) => {
                self.enter(&turn.thread_id);
                self.tools.clear();
                return;
            }
            ServerNotification::TurnCompleted(turn) => {
                self.enter(&turn.thread_id);
                self.tools.clear();
                return;
            }
            ServerNotification::ItemStarted(started) => {
                self.enter(&started.thread_id);
                if waits(&started.item) {
                    self.tools.insert(started.item.id().to_owned());
                }
                return;
            }
            ServerNotification::ItemCompleted(completed) => {
                self.enter(&completed.thread_id);
                self.tools.remove(completed.item.id());
                return;
            }
            ServerNotification::AgentMessageDelta(delta) => (&delta.thread_id, &delta.delta),
            ServerNotification::PlanDelta(delta) => (&delta.thread_id, &delta.delta),
            ServerNotification::ReasoningSummaryTextDelta(delta) => {
                (&delta.thread_id, &delta.delta)
            }
            ServerNotification::ReasoningTextDelta(delta) => (&delta.thread_id, &delta.delta),
            _ => return,
        };
        self.enter(thread);
        // About four bytes of streamed text make a token.
        self.carry += text.len();
        self.pending = self
            .pending
            .saturating_add(u32::try_from(self.carry / 4).unwrap_or(u32::MAX));
        self.carry %= 4;
    }

    fn enter(&mut self, thread: &str) {
        if self.thread != thread {
            thread.clone_into(&mut self.thread);
            self.tools.clear();
        }
    }

    fn spell(&self) -> Spell {
        if self.tools.is_empty() {
            Spell::Busy
        } else {
            Spell::Wait
        }
    }
}

/// Whether the turn waits on this item while it runs: a tool, not the model's own output.
fn waits(item: &ThreadItem) -> bool {
    matches!(
        item,
        ThreadItem::CodeMode(_)
            | ThreadItem::CommandExecution { .. }
            | ThreadItem::FileChange { .. }
            | ThreadItem::McpToolCall { .. }
            | ThreadItem::DynamicToolCall { .. }
            | ThreadItem::CollabAgentToolCall { .. }
            | ThreadItem::WebSearch(_)
            | ThreadItem::ImageView { .. }
            | ThreadItem::Sleep(_)
            | ThreadItem::ImageGeneration(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_windows_do_not_start_or_catch_up_the_shimmer_clock() {
        let (frames, _requests) = FrameRequester::test_channel();
        let mut live = Live {
            act: Act::Engaged(0),
            frames: Some(frames.clone()),
            ..Live::default()
        };
        let palette = Palette {
            fg: (208, 208, 208).into(),
            bg: (21, 21, 21).into(),
        };
        frames.set_mode(PresentationMode::Reduced);
        assert!(live.current(palette).is_none());
        assert!(matches!(live.act, Act::Engaged(_)));

        frames.set_mode(PresentationMode::Normal);
        assert!(live.current(palette).is_some());
        let before = Instant::now() - Duration::from_secs(20);
        let Act::Running { stepped, .. } = &mut live.act else {
            panic!("the foreground started the shimmer");
        };
        *stepped = before;
        // Content can still repaint an inactive window; painting must not run its clock.
        for mode in [PresentationMode::Reduced, PresentationMode::Frozen] {
            frames.set_mode(mode);
            assert!(live.current(palette).is_none());
            let Act::Running { stepped, .. } = &live.act else {
                panic!("attention changes retain the selected shimmer");
            };
            assert_eq!(*stepped, before);
        }
        frames.set_mode(PresentationMode::Normal);
        assert!(live.current(palette).is_some());
    }
}
