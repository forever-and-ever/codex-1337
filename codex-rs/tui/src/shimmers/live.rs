//! The TUI's own shimmer: one per process, fed by the displayed thread and drawn by its live rows.

use std::collections::HashMap;
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
use super::stage::DRAIN;
use super::stage::Palette;
use super::stage::Rgb;
use super::stage::Spell;
use super::stage::TICK;
use super::stage::pace;
use crate::motion::MotionMode;
use crate::terminal_palette::StdoutColorLevel;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::default_fg;
use crate::terminal_palette::effective_stdout_color_level;
use crate::tui::FrameRequester;
use crate::tui::PresentationMode;

/// A gap in drawing longer than this, as between turns, pauses the shimmer's clock.
const STALE: Duration = Duration::from_secs(/*secs*/ 1);
/// Output tokens a second the forecast assumes until a usage report teaches it.
const RATE: f64 = 60.0;
/// The weight of each usage report's rate in the forecast.
const LEARN: f64 = 0.3;
/// Bytes of serialized result per estimated token.
const BYTES_PER_TOKEN: f64 = 4.0;
/// Tokens one result may add by estimate; tools truncate what reaches the model, and the next usage
/// report settles the rest.
const RESULT_CAP: f64 = 10_000.0;

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
            self.feed.forget();
        }
        let Act::Running { engine, stepped } = &mut self.act else {
            return None;
        };
        if now.saturating_duration_since(*stepped) > STALE {
            *stepped = restart;
            self.feed.forget();
        }
        while now.saturating_duration_since(*stepped) >= tick {
            *stepped += tick;
            engine.step(self.feed.tick(), self.feed.spell());
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
    if let Some((thread, event)) = event(notification) {
        live().feed.observe(thread, event, Instant::now());
    }
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

/// The displayed thread's context inflow, the tokens entering its context, paced for the stage.
///
/// Usage reports count those tokens exactly, but only as each model response ends. Between reports
/// the feed estimates: while the model works it forecasts output at the rate recent responses
/// sustained, and a finished tool's result enters at its serialized size. Each report settles the
/// estimates since the one before against the response's output and the context's growth: a
/// shortfall joins the reservoir, which drains over [`DRAIN`] seconds, and an excess is taken back
/// from it. Inflow the stage did not see, while nothing was drawn, is forgotten, not replayed.
struct Feed {
    /// The thread that fed this last; another thread's notification starts the feed over.
    thread: String,
    phase: Phase,
    /// The last usage report, against which the next one settles; none after the feed forgets.
    usage: Option<Usage>,
    /// Seconds the model worked since the last usage report.
    worked: f64,
    /// Output tokens forecast since the last usage report.
    forecast: f64,
    /// Result tokens estimated since the last usage report.
    estimated: f64,
    /// Output tokens a second while the model works.
    rate: f64,
    /// Tokens yet to drain into the stage.
    reservoir: f64,
    /// Paced tokens short of a whole one.
    carry: f64,
}

impl Default for Feed {
    fn default() -> Self {
        Self {
            thread: String::new(),
            phase: Phase::Idle,
            usage: None,
            worked: 0.0,
            forecast: 0.0,
            estimated: 0.0,
            rate: RATE,
            reservoir: 0.0,
            carry: 0.0,
        }
    }
}

/// Where the displayed thread's turn stands.
enum Phase {
    /// No turn is known to run.
    Idle,
    /// The model works, as it has since `since`.
    Busy { since: Instant },
    /// Tools run, each with its serialized size when it started.
    Wait { tools: HashMap<String, usize> },
}

/// One model response's usage.
#[derive(Clone, Copy)]
struct Usage {
    /// Tokens of context the response read.
    context: i64,
    /// Tokens the response wrote.
    output: i64,
    /// The thread's tokens so far, which only a new response changes.
    total: i64,
}

/// A notification's bearing on the feed.
enum Event {
    TurnStarted,
    TurnCompleted,
    ToolStarted { id: String, size: usize },
    ToolCompleted { id: String, size: usize },
    Usage(Usage),
}

impl Feed {
    fn observe(&mut self, thread: &str, event: Event, now: Instant) {
        if self.thread != thread {
            *self = Feed {
                thread: thread.to_owned(),
                rate: self.rate,
                ..Feed::default()
            };
        }
        if let Phase::Busy { since } = &mut self.phase {
            self.worked += now.saturating_duration_since(*since).as_secs_f64();
            *since = now;
        }
        match event {
            Event::TurnStarted => self.phase = Phase::Busy { since: now },
            Event::TurnCompleted => self.phase = Phase::Idle,
            // A tool runs only within a turn, even one that started before the feed did.
            Event::ToolStarted { id, size } => match &mut self.phase {
                Phase::Wait { tools } => {
                    tools.insert(id, size);
                }
                Phase::Idle | Phase::Busy { .. } => {
                    self.phase = Phase::Wait {
                        tools: HashMap::from([(id, size)]),
                    };
                }
            },
            Event::ToolCompleted { id, size } => {
                let Phase::Wait { tools } = &mut self.phase else {
                    return;
                };
                if let Some(started) = tools.remove(&id) {
                    let result =
                        (size.saturating_sub(started) as f64 / BYTES_PER_TOKEN).min(RESULT_CAP);
                    self.estimated += result;
                    self.reservoir += result;
                }
                if tools.is_empty() {
                    self.phase = Phase::Busy { since: now };
                }
            }
            Event::Usage(usage) => self.settle(usage),
        }
    }

    fn settle(&mut self, usage: Usage) {
        if self.usage.is_some_and(|last| last.total == usage.total) {
            return;
        }
        let output = usage.output as f64;
        if let Some(last) = self.usage {
            let growth = (usage.context - last.context - last.output).max(0) as f64;
            self.reservoir =
                (self.reservoir + output - self.forecast + growth - self.estimated).max(0.0);
        }
        if self.worked >= 1.0 {
            self.rate += LEARN * (output / self.worked - self.rate);
        }
        self.usage = Some(usage);
        self.worked = 0.0;
        self.forecast = 0.0;
        self.estimated = 0.0;
    }

    /// Drops what the stage has not yet seen, as when the shimmer's clock restarts.
    fn forget(&mut self) {
        self.usage = None;
        self.reservoir = 0.0;
        self.carry = 0.0;
    }

    /// Tokens entering the context during one tick, as the feed reckons them.
    fn inflow(&mut self) -> f64 {
        let drained = self.reservoir * -(-TICK / DRAIN).exp_m1();
        self.reservoir -= drained;
        let forecast = match self.phase {
            Phase::Busy { .. } => self.rate * TICK,
            Phase::Idle | Phase::Wait { .. } => 0.0,
        };
        self.forecast += forecast;
        forecast + drained
    }

    /// One tick's paced tokens.
    fn tick(&mut self) -> u32 {
        self.carry += pace(self.inflow() / TICK) * TICK;
        let whole = self.carry.floor();
        self.carry -= whole;
        whole as u32
    }

    fn spell(&self) -> Spell {
        match self.phase {
            Phase::Wait { .. } => Spell::Wait,
            Phase::Idle | Phase::Busy { .. } => Spell::Busy,
        }
    }
}

/// What `notification` tells the feed, and the thread it concerns.
fn event(notification: &ServerNotification) -> Option<(&str, Event)> {
    Some(match notification {
        ServerNotification::TurnStarted(turn) => (turn.thread_id.as_str(), Event::TurnStarted),
        ServerNotification::TurnCompleted(turn) => (turn.thread_id.as_str(), Event::TurnCompleted),
        ServerNotification::ItemStarted(started) if waits(&started.item) => (
            started.thread_id.as_str(),
            Event::ToolStarted {
                id: started.item.id().to_owned(),
                size: size(&started.item),
            },
        ),
        ServerNotification::ItemCompleted(completed) if waits(&completed.item) => (
            completed.thread_id.as_str(),
            Event::ToolCompleted {
                id: completed.item.id().to_owned(),
                size: size(&completed.item),
            },
        ),
        ServerNotification::ThreadTokenUsageUpdated(updated) => (
            updated.thread_id.as_str(),
            Event::Usage(Usage {
                context: updated.token_usage.last.input_tokens,
                output: updated.token_usage.last.output_tokens,
                total: updated.token_usage.total.total_tokens,
            }),
        ),
        _ => return None,
    })
}

/// The length of `item`'s JSON, tallied without building it.
fn size(item: &ThreadItem) -> usize {
    struct Tally(usize);
    impl std::io::Write for Tally {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut tally = Tally(0);
    serde_json::to_writer(&mut tally, item).map_or(0, |()| tally.0)
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

    /// Tokens the feed lets into the stage over `secs`.
    fn run(feed: &mut Feed, secs: f64) -> f64 {
        (0..(secs / TICK).round() as usize)
            .map(|_| feed.inflow())
            .sum()
    }

    #[test]
    fn the_feed_lets_in_what_usage_reports_entered_the_context() {
        let start = Instant::now();
        let at = |secs: f64| start + Duration::from_secs_f64(secs);
        let mut feed = Feed::default();
        let thread = "thread";
        feed.observe(thread, Event::TurnStarted, at(/*secs*/ 0.0));
        // The first report has nothing earlier to settle against: it teaches the forecast's rate and
        // opens the books, and only the inflow after it is checked.
        run(&mut feed, /*secs*/ 10.0);
        let first = Usage {
            context: 10_000,
            output: 900,
            total: 10_900,
        };
        feed.observe(thread, Event::Usage(first), at(/*secs*/ 10.0));
        let started = Event::ToolStarted {
            id: "tool".to_owned(),
            size: 200,
        };
        feed.observe(thread, started, at(/*secs*/ 10.0));
        let mut inflow = run(&mut feed, /*secs*/ 5.0);
        // The result is estimated at 1000 tokens and enters the context as 1500.
        let completed = Event::ToolCompleted {
            id: "tool".to_owned(),
            size: 4_200,
        };
        feed.observe(thread, completed, at(/*secs*/ 15.0));
        inflow += run(&mut feed, /*secs*/ 10.0);
        let second = Usage {
            context: 12_400,
            output: 500,
            total: 12_900,
        };
        // A repeated report, as a rate-limit update sends, settles nothing twice.
        for _ in 0..2 {
            feed.observe(thread, Event::Usage(second), at(/*secs*/ 25.0));
        }
        feed.observe(thread, Event::TurnCompleted, at(/*secs*/ 25.0));
        inflow += run(&mut feed, /*secs*/ 60.0);
        // What the second report says entered: 500 tokens written and 1500 grown.
        assert!((inflow - 2_000.0).abs() < 1.0, "{inflow}");
    }
}
