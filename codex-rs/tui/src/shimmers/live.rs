//! The TUI's own shimmer: one per process, fed by the displayed thread and drawn by its live rows.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;

use codex_app_server_protocol::CodeModeOwnedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItemPayload;
use codex_protocol::ThreadId;
use codex_protocol::code_mode::CodeModeEvent;
use libshimmer::Glyphs;
use libshimmer::Intensity;
use libshimmer::Motion;
use libshimmer::Palette;
use libshimmer::Recipe;
use libshimmer::Reservoir;
use libshimmer::Row;
use libshimmer::STALE;
use libshimmer::Shimmer;
use libshimmer::Spell;
use libshimmer::TICK;
use libshimmer::stock::ROSTER;
use libshimmer_ratatui::InkExt as _;
use libshimmer_ratatui::RowExt as _;
use rand::seq::IteratorRandom as _;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Span;

use super::ears::Ears;
use super::playback;
use crate::motion::MotionMode;
use crate::terminal_palette::StdoutColorLevel;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::default_fg;
use crate::terminal_palette::effective_stdout_color_level;
use crate::tui::FrameRequester;
use crate::tui::PresentationMode;

/// Codex-tui's terminals draw octants.
pub(super) const GLYPHS: Glyphs = Glyphs::OCTANTS;
/// Tokens a second that count as `Intensity::BRISK`.
pub(super) const BRISK: f64 = 100.0;
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
    /// The status row painted last, whose backdrop `shade_status` lays beneath the rendered row.
    row: Row,
    pending_roll: Option<Arc<()>>,
}

#[derive(Default)]
enum Act {
    /// Nothing is engaged; the classic sweep runs.
    #[default]
    Classic,
    /// Drawing; `drawn` is when the shimmer last advanced, and `ears` hear for a shimmer that
    /// listens, once `PipeWire` lets them.
    Running {
        shimmer: Box<Shimmer>,
        drawn: Instant,
        ears: Option<Ears>,
    },
}

impl Live {
    fn draws_motion(&self) -> bool {
        self.frames
            .as_ref()
            .is_some_and(|frames| frames.mode() == PresentationMode::Normal)
    }

    /// Whether the status row moves: focused, or visible through its glints.
    fn glints(&self) -> bool {
        self.frames
            .as_ref()
            .is_some_and(|frames| frames.mode().glints())
    }

    #[expect(
        clippy::expect_used,
        reason = "Codex-tui engages only the stock recipes it offers, which fit its wide captions and octants."
    )]
    fn engage(&mut self, recipe: &'static Recipe, ears: Option<Ears>) {
        let shimmer = Shimmer::new(recipe, rand::random(), GLYPHS)
            .expect("the selected stock recipe fits codex-tui");
        self.act = Act::Running {
            shimmer: Box::new(shimmer),
            drawn: Instant::now(),
            ears,
        };
        self.feed.forget();
        self.row = Row::default();
    }

    fn engaged(&self) -> Option<&'static Recipe> {
        match &self.act {
            Act::Classic => None,
            Act::Running { shimmer, .. } => Some(shimmer.recipe()),
        }
    }

    /// Tells the shimmer its bout ended.
    fn idle(&mut self) {
        if let Act::Running { shimmer, .. } = &mut self.act {
            shimmer.set_spell(Spell::Idle);
        }
    }

    /// The running shimmer, advanced up to now.
    fn current(&mut self) -> Option<&mut Shimmer> {
        if !self.glints() {
            return None;
        }
        let now = Instant::now();
        let Act::Running {
            shimmer,
            drawn,
            ears,
        } = &mut self.act
        else {
            return None;
        };
        let dt = now.saturating_duration_since(*drawn);
        let measured = if dt > STALE {
            self.feed.forget();
            TICK
        } else {
            dt
        };
        shimmer.set_spell(self.feed.spell());
        shimmer.set_intensity(self.feed.intensity(measured));
        if let Some(ears) = ears {
            shimmer.hear(&ears.drain());
        }
        shimmer.advance(dt);
        *drawn = now;
        Some(shimmer)
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

/// Begins a new roll immediately so that later selections can supersede it during the probe.
pub(crate) fn engage_random() -> impl Future<Output = ()> {
    let ticket = Arc::new(());
    let current = {
        let mut live = live();
        live.pending_roll = Some(Arc::clone(&ticket));
        live.engaged()
    };
    async move {
        let candidates = ROSTER.iter().filter(|recipe| {
            offered(recipe) && !current.is_some_and(|current| std::ptr::eq(*recipe, current))
        });
        let drawn = candidates.clone().choose(&mut rand::rng());
        let recipe = if let Some(drawn) = drawn {
            resolve_roll(drawn, candidates, playback::playing()).await
        } else {
            None
        };
        let ears = if recipe.is_some_and(Recipe::listens) {
            tokio::task::spawn_blocking(Ears::open).await.ok().flatten()
        } else {
            None
        };
        let mut live = live();
        if live
            .pending_roll
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, &ticket))
        {
            live.pending_roll = None;
            if let Some(recipe) = recipe {
                live.engage(recipe, ears);
                if let Some(frames) = &live.frames {
                    frames.schedule_frame();
                }
            }
        }
    }
}

async fn resolve_roll(
    recipe: &'static Recipe,
    candidates: impl Iterator<Item = &'static Recipe>,
    playback: impl Future<Output = bool>,
) -> Option<&'static Recipe> {
    if recipe.listens() && !playback.await {
        // Redrawing uniformly from the remaining recipes preserves their relative odds.
        candidates
            .filter(|recipe| !recipe.listens())
            .choose(&mut rand::rng())
    } else {
        Some(recipe)
    }
}

pub(super) fn engage(recipe: &'static Recipe) {
    let ears = recipe.listens().then(Ears::open).flatten();
    let mut live = live();
    live.pending_roll = None;
    live.engage(recipe, ears);
}

/// Whether codex-tui offers `recipe`: it fits the terminal, and it listens only where `PipeWire`
/// lets it hear.
pub(super) fn offered(recipe: &Recipe) -> bool {
    recipe.fits(GLYPHS).is_ok() && (!recipe.listens() || Ears::can_hear())
}

/// Held for the TUI's session: dropping it stops the engaged shimmer's ears, so that no recorder
/// outlives the TUI.
pub(crate) struct Earplug;

impl Drop for Earplug {
    fn drop(&mut self) {
        let mut live = live();
        live.pending_roll = None;
        if let Act::Running { ears, .. } = &mut live.act {
            *ears = None;
        }
    }
}

/// The recipe engaged, if any.
pub(super) fn engaged() -> Option<&'static Recipe> {
    live().engaged()
}

/// Feeds the shimmer from a live notification, which bears on it only from the displayed thread.
pub(crate) fn observe(notification: &ServerNotification) {
    let Some((thread, event)) = event(notification) else {
        return;
    };
    let mut live = live();
    if live.feed.follows(thread) && live.feed.observe(event, Instant::now()) {
        live.idle();
    }
}

/// Follows the displayed thread and whether its turn runs, as its chat widget knows them before
/// each frame: a replay or a switch restores a turn no live notification announced.
pub(crate) fn follow_thread(thread: Option<ThreadId>, running: bool) {
    let mut live = live();
    if live.feed.display(thread, running, Instant::now()) {
        live.idle();
    }
}

/// Replaces the status row's lead in `spans`, turning the upstream header into a caption.
/// Leaves `spans` alone when nothing is engaged, nothing moves, or colors are unknown.
/// True when the engaged shimmer painted the spans.
pub(crate) fn dress_status(
    spans: &mut Vec<Span<'static>>,
    caption: &str,
    mark: MotionMode,
    letters: MotionMode,
) -> bool {
    let mut live = live();
    live.row = Row::default();
    let (mark, letters) = (
        mark == MotionMode::Animated,
        letters == MotionMode::Animated,
    );
    if !(mark || letters) {
        return false;
    }
    let Some(palette) = palette() else {
        return false;
    };
    let Some(shimmer) = live.current() else {
        return false;
    };
    shimmer.show(caption);
    let row = shimmer.status(palette, Motion { mark, letters });
    *spans = row.spans();
    live.row = row;
    true
}

/// Lays the dressed status row's backdrop under the row rendered at the top of `area`.
pub(crate) fn shade_status(area: Rect, buf: &mut Buffer) {
    let live = live();
    if live.glints() {
        live.row.shade(area, buf);
    }
}

pub(crate) fn bullet() -> Option<Span<'static>> {
    let mut live = live();
    if !live.draws_motion() {
        return (!matches!(live.act, Act::Classic)).then(|| "•".dim());
    }
    let palette = palette()?;
    live.current()
        .map(|shimmer| shimmer.bullet(palette).span("•"))
}

/// The displayed thread's context inflow, the tokens entering its context, measured for the shimmer.
///
/// Usage reports count those tokens exactly, but only as each model response ends, once the calls it
/// made have answered. Between reports the feed estimates: while the model works it forecasts output
/// at the rate recent responses sustained; while tool calls hold the turn, their commands' output
/// enters as it streams, and a finished tool's result at its serialized size, less what streamed. A
/// Code Mode cell holds the turn as a call too, whose result only the next report measures. Each
/// report settles the estimates since the one before against the response's output and the
/// context's growth: a shortfall joins the reservoir, which drains over four seconds, and an excess
/// stays shown, since taking it back would still the stage after every response slow to start.
/// Inflow the stage did not see, while nothing was drawn, is forgotten, not replayed.
struct Feed {
    /// The displayed thread, whose notifications alone feed this.
    thread: Option<ThreadId>,
    phase: Phase,
    books: Books,
    /// Seconds the model worked since the last usage report.
    worked: f64,
    /// Output tokens forecast since the last usage report.
    forecast: f64,
    /// Result tokens estimated since the last usage report.
    estimated: f64,
    /// Tokens each call's output stream let in this turn, which its result must not count again.
    streamed: HashMap<String, f64>,
    /// Output tokens a second while the model works.
    rate: f64,
    /// Tokens yet to drain into the stage.
    reservoir: Reservoir,
}

impl Default for Feed {
    fn default() -> Self {
        Self {
            thread: None,
            phase: Phase::Idle,
            books: Books::Unopened,
            worked: 0.0,
            forecast: 0.0,
            estimated: 0.0,
            streamed: HashMap::new(),
            rate: RATE,
            reservoir: Reservoir::default(),
        }
    }
}

/// Where the displayed thread's turn stands.
enum Phase {
    /// No turn is known to run.
    Idle,
    /// The model works, as it has since `since`.
    Busy { since: Instant },
    /// Tool calls hold the turn, by call id, each with its item's serialized size when it started,
    /// which its result grows; a Code Mode cell, and a wait on one, has none.
    Wait {
        calls: HashMap<String, Option<usize>>,
    },
}

/// The usage reports the feed has read.
#[derive(Clone, Copy)]
enum Books {
    /// None since the feed followed its thread.
    Unopened,
    /// The next report settles against this one.
    Open(Usage),
    /// The stage forgot what was pending since this report, so the next one settles nothing.
    Forgotten(Usage),
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
#[derive(Clone, Copy)]
enum Event<'n> {
    TurnStarted,
    TurnCompleted,
    /// A tool call began holding the turn, `size` as for [`Phase::Wait`].
    ToolStarted {
        id: &'n str,
        size: Option<usize>,
    },
    /// A command's output streamed `bytes` more under the call `id`.
    ToolOutput {
        id: &'n str,
        bytes: usize,
    },
    ToolCompleted {
        id: &'n str,
        size: Option<usize>,
    },
    Usage(Usage),
}

impl Feed {
    /// Whether `thread` names the displayed thread.
    fn follows(&self, thread: &str) -> bool {
        self.thread.is_some() && ThreadId::from_string(thread).ok() == self.thread
    }

    /// Follows the displayed `thread`, starting over when it is another, and whether its turn
    /// runs. True when that ends a bout.
    fn display(&mut self, thread: Option<ThreadId>, running: bool, now: Instant) -> bool {
        if self.thread != thread {
            *self = Feed {
                thread,
                rate: self.rate,
                ..Feed::default()
            };
        }
        let bout = !matches!(self.phase, Phase::Idle);
        if running && !bout {
            self.phase = Phase::Busy { since: now };
        }
        let ended = bout && !running;
        if ended {
            self.phase = Phase::Idle;
        }
        ended
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "Byte counts become approximate visual token estimates, capped by RESULT_CAP; exact accounting remains upstream."
    )]
    fn observe(&mut self, event: Event<'_>, now: Instant) -> bool {
        let ended = !matches!(self.phase, Phase::Idle) && matches!(event, Event::TurnCompleted);
        // A turn's start proves any bout the feed still believed in over, not worked.
        if !matches!(event, Event::TurnStarted)
            && let Phase::Busy { since } = &mut self.phase
        {
            self.worked += now.saturating_duration_since(*since).as_secs_f64();
            *since = now;
        }
        match event {
            Event::TurnStarted => {
                self.streamed.clear();
                self.phase = Phase::Busy { since: now };
            }
            Event::TurnCompleted => self.phase = Phase::Idle,
            // A tool call holds only a bout's turn.
            Event::ToolStarted { id, size } => match &mut self.phase {
                Phase::Idle => {}
                Phase::Busy { .. } => {
                    self.phase = Phase::Wait {
                        calls: HashMap::from([(id.to_owned(), size)]),
                    };
                }
                Phase::Wait { calls } => {
                    calls.insert(id.to_owned(), size);
                }
            },
            Event::ToolOutput { id, bytes } => {
                if matches!(self.phase, Phase::Wait { .. }) {
                    let streamed = self.streamed.entry(id.to_owned()).or_default();
                    let tokens = (bytes as f64 / BYTES_PER_TOKEN).min(RESULT_CAP - *streamed);
                    *streamed += tokens;
                    self.estimated += tokens;
                    self.reservoir.pour(tokens);
                }
            }
            Event::ToolCompleted { id, size } => {
                let Phase::Wait { calls } = &mut self.phase else {
                    return false;
                };
                if let Some(Some(started)) = calls.remove(id)
                    && let Some(size) = size
                {
                    let result =
                        (size.saturating_sub(started) as f64 / BYTES_PER_TOKEN).min(RESULT_CAP);
                    // What streamed while the call ran has entered already.
                    let rest = (result - self.streamed.remove(id).unwrap_or_default()).max(0.0);
                    self.estimated += rest;
                    self.reservoir.pour(rest);
                }
                if calls.is_empty() {
                    self.phase = Phase::Busy { since: now };
                }
            }
            Event::Usage(usage) => self.settle(usage, now),
        }
        ended
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "Usage counts only calibrate an approximate animation rate; no accounting depends on these floating-point estimates."
    )]
    fn settle(&mut self, usage: Usage, now: Instant) {
        let last = match self.books {
            Books::Open(last) | Books::Forgotten(last) if last.total == usage.total => return,
            Books::Open(last) => Some(last),
            Books::Unopened | Books::Forgotten(_) => None,
        };
        // Core reports a response's usage only once its calls answered: one still open was
        // backgrounded, and the model samples again.
        if matches!(self.phase, Phase::Wait { .. }) {
            self.phase = Phase::Busy { since: now };
        }
        let output = usage.output as f64;
        if let Some(last) = last {
            let growth = (usage.context - last.context - last.output).max(0) as f64;
            // A shortfall joins the reservoir; an excess the stage already showed stays shown.
            self.reservoir
                .pour((output - self.forecast + growth - self.estimated).max(0.0));
        }
        if self.worked >= 1.0 {
            self.rate += LEARN * (output / self.worked - self.rate);
        }
        self.books = Books::Open(usage);
        self.worked = 0.0;
        self.forecast = 0.0;
        self.estimated = 0.0;
    }

    /// Drops what the stage has not yet seen, as when the shimmer's clock restarts.
    fn forget(&mut self) {
        if let Books::Open(last) = self.books {
            self.books = Books::Forgotten(last);
        }
        self.reservoir = Reservoir::default();
    }

    /// The context's inflow rate over `dt`, normalized to brisk work.
    fn intensity(&mut self, dt: Duration) -> Intensity {
        let drained = self.reservoir.drain(dt);
        let seconds = dt.as_secs_f64();
        let forecast = match self.phase {
            Phase::Busy { .. } => self.rate * seconds,
            Phase::Idle | Phase::Wait { .. } => 0.0,
        };
        self.forecast += forecast;
        Intensity::new((forecast + drained) / seconds / BRISK)
    }

    fn spell(&self) -> Spell {
        match self.phase {
            Phase::Wait { .. } => Spell::Wait,
            Phase::Idle => Spell::Idle,
            Phase::Busy { .. } => Spell::Busy,
        }
    }
}

/// What `notification` tells the feed, and the thread it concerns.
fn event(notification: &ServerNotification) -> Option<(&str, Event<'_>)> {
    Some(match notification {
        ServerNotification::TurnStarted(turn) => (turn.thread_id.as_str(), Event::TurnStarted),
        ServerNotification::TurnCompleted(turn) => (turn.thread_id.as_str(), Event::TurnCompleted),
        ServerNotification::ItemStarted(started) if waits(&started.item.payload) => (
            started.thread_id.as_str(),
            Event::ToolStarted {
                id: started.item.id(),
                size: Some(size(&started.item.payload)),
            },
        ),
        ServerNotification::ItemCompleted(completed) if waits(&completed.item.payload) => (
            completed.thread_id.as_str(),
            Event::ToolCompleted {
                id: completed.item.id(),
                size: Some(size(&completed.item.payload)),
            },
        ),
        ServerNotification::CommandExecutionOutputDelta(delta) => (
            delta.thread_id.as_str(),
            Event::ToolOutput {
                id: &delta.item_id,
                bytes: delta.delta.len(),
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
        ServerNotification::CodeModeOwned(owned) => return cell(owned),
        _ => return None,
    })
}

/// What a Code Mode cell's notification tells the feed: the cell, and each wait the model calls on
/// one, holds the turn as a call, and the output of its commands streams under the cell's call.
fn cell(owned: &CodeModeOwnedNotification) -> Option<(&str, Event<'_>)> {
    match owned.payload().as_ref() {
        ServerNotification::CommandExecutionOutputDelta(delta) => Some((
            delta.thread_id.as_str(),
            Event::ToolOutput {
                id: &owned.owner.exec_call_id,
                bytes: delta.delta.len(),
            },
        )),
        ServerNotification::ItemCompleted(completed) => {
            let ThreadItemPayload::CodeMode(fact) = &completed.item.payload else {
                return None;
            };
            let event = match &fact.event {
                CodeModeEvent::Started { .. } => Event::ToolStarted {
                    id: &fact.exec_call_id,
                    size: None,
                },
                // A wait the model called holds the turn; a cell's own wait on a process does not.
                CodeModeEvent::WaitStarted {
                    call_id,
                    target: None,
                } => Event::ToolStarted {
                    id: call_id,
                    size: None,
                },
                CodeModeEvent::WaitCompleted {
                    call_id,
                    target: None,
                } => Event::ToolCompleted {
                    id: call_id,
                    size: None,
                },
                CodeModeEvent::Finished { .. } => Event::ToolCompleted {
                    id: &fact.exec_call_id,
                    size: None,
                },
                CodeModeEvent::Allocated
                | CodeModeEvent::ToolCall { .. }
                | CodeModeEvent::WaitStarted { .. }
                | CodeModeEvent::WaitCompleted { .. } => return None,
            };
            Some((completed.thread_id.as_str(), event))
        }
        _ => None,
    }
}

/// The length of `item`'s JSON, tallied without building it.
fn size(item: &ThreadItemPayload) -> usize {
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
fn waits(item: &ThreadItemPayload) -> bool {
    matches!(
        item,
        ThreadItemPayload::CommandExecution { .. }
            | ThreadItemPayload::FileChange { .. }
            | ThreadItemPayload::McpToolCall { .. }
            | ThreadItemPayload::DynamicToolCall { .. }
            | ThreadItemPayload::CollabAgentToolCall { .. }
            | ThreadItemPayload::WebSearch(_)
            | ThreadItemPayload::ImageView { .. }
            | ThreadItemPayload::Sleep(_)
            | ThreadItemPayload::ImageGeneration(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn random_probes_playback_only_for_a_listening_recipe() {
        let playback = std::future::ready;
        let Some(hi_fi) = ROSTER.iter().find(|recipe| recipe.listens()) else {
            unreachable!("the stock roster contains a listener");
        };
        let Some(silent) = ROSTER.iter().find(|recipe| !recipe.listens()) else {
            unreachable!("the stock roster contains a non-listener");
        };
        let candidates = [hi_fi, silent];
        assert!(
            resolve_roll(hi_fi, candidates.into_iter(), playback(false))
                .await
                .is_some_and(|recipe| std::ptr::eq(recipe, silent))
        );
        assert!(
            resolve_roll(hi_fi, candidates.into_iter(), playback(true))
                .await
                .is_some_and(|recipe| std::ptr::eq(recipe, hi_fi))
        );
        let mut queried = false;
        assert!(
            resolve_roll(silent, candidates.into_iter(), async {
                queried = true;
                false
            })
            .await
            .is_some_and(|recipe| std::ptr::eq(recipe, silent))
        );
        assert!(!queried);
    }

    #[test]
    fn inactive_windows_do_not_start_or_catch_up_the_shimmer_clock() {
        let (frames, _requests) = FrameRequester::test_channel();
        let before = Instant::now().checked_sub(Duration::from_secs(20)).unwrap();
        let mut live = Live {
            act: Act::Running {
                shimmer: Box::new(Shimmer::new(&ROSTER[0], 7, GLYPHS).unwrap()),
                drawn: before,
                ears: None,
            },
            frames: Some(frames.clone()),
            ..Live::default()
        };
        live.feed.observe(Event::TurnStarted, before);
        // Content can still repaint an inactive window; painting must not run its clock or feed.
        for mode in [PresentationMode::Reduced, PresentationMode::Frozen] {
            frames.set_mode(mode);
            assert!(live.current().is_none());
            let Act::Running { drawn, .. } = &live.act else {
                panic!("attention changes retain the selected shimmer");
            };
            assert_eq!(*drawn, before);
            assert_eq!(live.feed.forecast, 0.0);
        }
        frames.set_mode(PresentationMode::Normal);
        assert!(live.current().is_some());
        // Returning to the foreground measures one tick, never the hidden twenty seconds.
        assert!((live.feed.forecast - RATE * TICK.as_secs_f64()).abs() < f64::EPSILON);
        let Act::Running { drawn, .. } = &live.act else {
            panic!("the foreground retains the shimmer");
        };
        assert!(*drawn > before);
    }

    /// Tokens the feed lets into the stage over `secs`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "The test deliberately quantizes a nonnegative fixed duration into animation ticks."
    )]
    fn run(feed: &mut Feed, secs: f64) -> f64 {
        (0..(secs / TICK.as_secs_f64()).round() as usize)
            .map(|_| feed.intensity(TICK).level() * BRISK * TICK.as_secs_f64())
            .sum()
    }

    #[test]
    fn the_feed_lets_in_what_usage_reports_entered_the_context() {
        let start = Instant::now();
        let at = |secs: f64| start + Duration::from_secs_f64(secs);
        let mut feed = Feed::default();
        feed.observe(Event::TurnStarted, at(/*secs*/ 0.0));
        // The first report has nothing earlier to settle against: it teaches the forecast's rate and
        // opens the books, and only the inflow after it is checked.
        run(&mut feed, /*secs*/ 10.0);
        let first = Usage {
            context: 10_000,
            output: 900,
            total: 10_900,
        };
        feed.observe(Event::Usage(first), at(/*secs*/ 10.0));
        let started = Event::ToolStarted {
            id: "tool",
            size: Some(200),
        };
        feed.observe(started, at(/*secs*/ 10.0));
        // Output streams in as the command runs, and its result then lets in only the rest.
        let output = Event::ToolOutput {
            id: "tool",
            bytes: 2_400,
        };
        feed.observe(output, at(/*secs*/ 12.0));
        let mut inflow = run(&mut feed, /*secs*/ 5.0);
        // The result is estimated at 1000 tokens and enters the context as 1500.
        let completed = Event::ToolCompleted {
            id: "tool",
            size: Some(4_200),
        };
        feed.observe(completed, at(/*secs*/ 15.0));
        inflow += run(&mut feed, /*secs*/ 10.0);
        let second = Usage {
            context: 12_400,
            output: 500,
            total: 12_900,
        };
        // A repeated report, as a rate-limit update sends, settles nothing twice.
        for _ in 0..2 {
            feed.observe(Event::Usage(second), at(/*secs*/ 25.0));
        }
        assert!(feed.observe(Event::TurnCompleted, at(/*secs*/ 25.0)));
        assert!(!feed.observe(Event::TurnCompleted, at(/*secs*/ 25.0)));
        inflow += run(&mut feed, /*secs*/ 60.0);
        // What the second report says entered: 500 tokens written and 1500 grown.
        assert!((inflow - 2_000.0).abs() < 1.0, "{inflow}");
    }
}
