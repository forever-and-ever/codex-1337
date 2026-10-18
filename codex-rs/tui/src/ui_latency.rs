//! Worker-local, content-free dispatcher wall time. Never a CPU profiler.
mod endpoint;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::atomic::fence;
use std::time::Instant;

use crossterm::event::MouseEventKind;

pub use endpoint::discover;
pub use endpoint::query;

const BUCKETS: usize = 12;
const SLOW: usize = 128;
const DEPTH: usize = 8;
const STAT_WORDS: usize = 4 + BUCKETS;
const SLOW_NS: u64 = 8_000_000;
pub const BUCKET_UPPER_NS: [u64; BUCKETS] = [
    100_000,
    250_000,
    500_000,
    1_000_000,
    2_000_000,
    4_000_000,
    8_000_000,
    16_000_000,
    50_000_000,
    100_000_000,
    500_000_000,
    u64::MAX,
];

macro_rules! stages {
    (active { $($name:ident = $id:literal),+ $(,)? }
     retired { $($retired:ident = $retired_id:literal),* $(,)? }) => {
        #[derive(Clone, Copy, Debug)]
        #[repr(usize)]
        pub(crate) enum Stage { $($name = $id),+ }
        pub const STAGE_NAMES: [&str; [$(stringify!($name)),+, $(stringify!($retired)),*].len()] = {
            let mut names = [""; [$(stringify!($name)),+, $(stringify!($retired)),*].len()];
            $(names[$id] = stringify!($name);)+
            $(names[$retired_id] = stringify!($retired);)*
            names
        };
    };
}
// Protocol v1 IDs are permanent. Append IDs; never reuse or renumber them.
stages! {
    active {
    Startup = 0, Maintenance = 1, IdleSelect = 2, AppEvent = 3, ThreadEvent = 4,
    TerminalEvent = 5, ServerCompletion = 6, DeliveryCompletion = 7, ServerEvent = 8,
    Reconnect = 9, Timer = 10, KeyRouting = 11,
    FramePreparation = 13, FramePaint = 14, InvocationPreparation = 15,
    TerminalDiff = 16, TerminalWrite = 17, TerminalFlush = 18,
    ReactionLatency = 20,
    }
    retired { PreKeyPaint = 12, RpcWait = 19 }
}
const STAGES: usize = STAGE_NAMES.len();

// Every concurrently read word is atomic, including words in rejected snapshots.
// Release fence after odd + acquire fence after payload prevents accepting a
// mixture: observing any new payload forces the final version read past odd.
// Acquire initial version + release even prevents accepting unfinished old data.
// One attempt only: a suspended publisher never obstructs a diagnostic reader.
struct Record<const N: usize> {
    version: AtomicU64,
    words: [AtomicU64; N],
}
impl<const N: usize> Record<N> {
    fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
            words: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
    fn publish(&self, update: impl FnOnce(&[AtomicU64; N])) {
        let version = self.version.load(Ordering::Relaxed);
        self.version
            .store(version.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        update(&self.words);
        self.version
            .store(version.wrapping_add(2), Ordering::Release);
    }
    fn store(&self, values: [u64; N]) {
        self.publish(|words| {
            for (word, value) in words.iter().zip(values) {
                word.store(value, Ordering::Relaxed);
            }
        });
    }
    fn read(&self) -> Option<[u64; N]> {
        let before = self.version.load(Ordering::Acquire);
        if before & 1 != 0 {
            return None;
        }
        let values = std::array::from_fn(|index| self.words[index].load(Ordering::Relaxed));
        fence(Ordering::Acquire);
        (before == self.version.load(Ordering::Relaxed)).then_some(values)
    }
}
struct Shared {
    origin: Instant,
    requested_epoch: AtomicU64,
    active_epoch: AtomicU64,
    current: Record<7>,
    committed: Record<5>,
    admission: Record<7>,
    last_input_ns: AtomicU64,
    suppressed_scopes: AtomicU64,
    stats: [Record<STAT_WORDS>; STAGES],
    slow: [Record<5>; SLOW],
}
impl Shared {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
            requested_epoch: AtomicU64::new(1),
            active_epoch: AtomicU64::new(1),
            current: Record::new(),
            committed: Record::new(),
            admission: Record::new(),
            last_input_ns: AtomicU64::new(0),
            suppressed_scopes: AtomicU64::new(0),
            stats: std::array::from_fn(|_| Record::new()),
            slow: std::array::from_fn(|_| Record::new()),
        }
    }
    fn now(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}
#[derive(Clone, Copy)]
struct Span {
    stage: Stage,
    start: u64,
    epoch: u64,
}

struct Writer {
    shared: Arc<Shared>,
    epoch: u64,
    stack: [Span; DEPTH],
    depth: usize,
    stats: [[u64; STAT_WORDS]; STAGES],
    completed: u64,
    suppressed_scopes: u64,
    slow_count: u64,
    gate: InputGate,
    gate_since: u64,
    ready_input: Option<Span>,
    armed_input: Option<Span>,
}
impl Writer {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            epoch: 1,
            stack: [Span {
                stage: Stage::Startup,
                start: 0,
                epoch: 1,
            }; DEPTH],
            depth: 0,
            stats: [[0; STAT_WORDS]; STAGES],
            completed: 0,
            suppressed_scopes: 0,
            slow_count: 0,
            gate: InputGate::Open,
            gate_since: 0,
            ready_input: None,
            armed_input: None,
        }
    }
    fn activate_reset(&mut self) {
        let epoch = self.shared.requested_epoch.load(Ordering::Relaxed);
        if epoch == self.epoch {
            return;
        }
        self.epoch = epoch;
        self.stats = [[0; STAT_WORDS]; STAGES];
        self.completed = 0;
        self.suppressed_scopes = 0;
        self.shared.suppressed_scopes.store(0, Ordering::Relaxed);
        self.slow_count = 0;
        self.ready_input = None;
        self.armed_input = None;
        // Old shared rows need not be cleared: each row carries its epoch.
        self.shared.active_epoch.store(epoch, Ordering::Release);
    }
    fn current(&self, now: u64) {
        let top = self.depth.checked_sub(1).map(|i| self.stack[i]);
        self.shared.current.store([
            self.epoch,
            now,
            top.map_or(0, |s| s.stage as u64 + 1),
            top.map_or(0, |s| s.start),
            if self.depth > 1 {
                self.stack[self.depth - 2].stage as u64 + 1
            } else {
                0
            },
            self.depth as u64,
            top.map_or(self.epoch, |s| s.epoch),
        ]);
    }
    fn enter(&mut self, stage: Stage) -> Option<usize> {
        self.activate_reset();
        if self.depth == DEPTH {
            self.suppressed_scopes = self.suppressed_scopes.saturating_add(1);
            self.shared
                .suppressed_scopes
                .store(self.suppressed_scopes, Ordering::Relaxed);
            return None;
        }
        if matches!(stage, Stage::FramePaint) && self.armed_input.is_none() {
            self.armed_input = self.ready_input.take();
        }
        let depth = self.depth;
        let now = self.shared.now();
        self.stack[depth] = Span {
            stage,
            start: now,
            epoch: self.epoch,
        };
        self.depth += 1;
        self.current(now);
        Some(depth)
    }
    fn leave(&mut self, depth: usize) {
        assert_eq!(self.depth, depth + 1, "UI latency scopes must be nested");
        self.activate_reset();
        let span = self.stack[depth];
        let now = self.shared.now();
        self.depth = depth;
        self.record(span, now);
        if matches!(span.stage, Stage::TerminalFlush)
            && let Some(input) = self.armed_input.take()
        {
            self.record(input, now);
        }
        self.current(now);
    }
    fn record(
        &mut self,
        Span {
            stage,
            start,
            epoch,
        }: Span,
        now: u64,
    ) {
        if epoch != self.epoch {
            return;
        }
        let elapsed = now.saturating_sub(start);
        let bucket = BUCKET_UPPER_NS.partition_point(|upper| *upper < elapsed);
        let stats = &mut self.stats[stage as usize];
        stats[0] = epoch;
        stats[1] = stats[1].saturating_add(1);
        stats[2] = stats[2].saturating_add(elapsed);
        stats[3] = stats[3].max(elapsed);
        stats[4 + bucket] = stats[4 + bucket].saturating_add(1);
        self.shared.stats[stage as usize].publish(|words| {
            // Reset activates lazily per row; old histogram buckets must
            // disappear before the first new-epoch completion is accepted.
            if words[0].load(Ordering::Relaxed) != epoch {
                for word in &words[4..] {
                    word.store(0, Ordering::Relaxed);
                }
            }
            for index in 0..4 {
                words[index].store(stats[index], Ordering::Relaxed);
            }
            words[4 + bucket].store(stats[4 + bucket], Ordering::Relaxed);
        });
        self.completed = self.completed.saturating_add(1);
        self.shared
            .committed
            .store([epoch, now, stage as u64 + 1, elapsed, self.completed]);
        let slow_ns = if matches!(stage, Stage::ReactionLatency) {
            16_700_000
        } else {
            SLOW_NS
        };
        if elapsed >= slow_ns && !matches!(stage, Stage::IdleSelect) {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Modulo SLOW yields an index strictly below the fixed ring length"
            )]
            let slow_slot = (self.slow_count % SLOW as u64) as usize;
            self.shared.slow[slow_slot].store([
                epoch,
                self.slow_count.saturating_add(1),
                stage as u64 + 1,
                start,
                elapsed,
            ]);
            self.slow_count = self.slow_count.saturating_add(1);
        }
    }
}

tokio::task_local! { static WRITER: RefCell<Writer>; }

// Tokio task-local scope follows the dispatcher future across thread migration;
// spawned tasks do NOT inherit it. Thus helper seams outside this one UI task
// cannot acquire writer authority. Guards never hold a RefCell borrow over work.
pub(crate) struct Guard(Option<usize>);
pub(crate) fn enter(stage: Stage) -> Guard {
    Guard(
        WRITER
            .try_with(|writer| writer.borrow_mut().enter(stage))
            .ok()
            .flatten(),
    )
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(depth) = self.0 {
            let _ = WRITER.try_with(|writer| writer.borrow_mut().leave(depth));
        }
    }
}
pub(crate) fn admission(
    gate: InputGate,
    app_backlog: usize,
    thread_backlog: usize,
    primary_backlog: usize,
) {
    let _ = WRITER.try_with(|writer| {
        let mut writer = writer.borrow_mut();
        writer.activate_reset();
        let now = writer.shared.now();
        if writer.gate != gate {
            writer.gate_since = now;
            writer.gate = gate;
        }
        writer.shared.admission.store([
            writer.epoch,
            now,
            gate as u64,
            app_backlog as u64,
            writer.gate_since,
            thread_backlog as u64,
            primary_backlog as u64,
        ]);
    });
}
pub(crate) async fn scope<F: Future>(future: F) -> F::Output {
    let shared = Arc::new(Shared::new());
    let _endpoint = match endpoint::Endpoint::start(Arc::clone(&shared)) {
        Ok(endpoint) => Some(endpoint),
        Err(error) => {
            tracing::warn!(%error, "UI latency endpoint unavailable; UI continues without remote diagnostics");
            None
        }
    };
    WRITER
        .scope(RefCell::new(Writer::new(shared)), future)
        .await
}

pub use endpoint::Report;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub(crate) enum InputGate {
    Open,
    ThreadSwitch,
    Replay,
    Startup,
}

pub(crate) struct InputStamp(Span);

/// Owned by a submitted frame until the UI receives its flush completion.
pub(crate) struct FrameStamp {
    epoch: u64,
    input: Option<Span>,
}

pub(crate) fn frame_submitted() -> Option<FrameStamp> {
    WRITER
        .try_with(|writer| {
            let mut writer = writer.borrow_mut();
            writer.activate_reset();
            FrameStamp {
                epoch: writer.epoch,
                input: writer.armed_input.take(),
            }
        })
        .ok()
}

pub(crate) fn merge_frame_stamps(
    old: Option<FrameStamp>,
    new: Option<FrameStamp>,
) -> Option<FrameStamp> {
    match (old, new) {
        (Some(old), Some(mut new)) if old.epoch == new.epoch => {
            if let Some(input) = old.input
                && new.input.is_none_or(|pending| input.start < pending.start)
            {
                new.input = Some(input);
            }
            Some(new)
        }
        (_, Some(new)) => Some(new),
        (old, None) => old,
    }
}

pub(crate) fn frame_completed(
    stamp: Option<FrameStamp>,
    phases: [(Instant, Instant); 3],
    ended_at: Instant,
) {
    let Some(stamp) = stamp else { return };
    let _ = WRITER.try_with(|writer| {
        let mut writer = writer.borrow_mut();
        writer.activate_reset();
        if stamp.epoch != writer.epoch {
            return;
        }
        let origin = writer.shared.origin;
        let nanos = |at: Instant| {
            u64::try_from(at.saturating_duration_since(origin).as_nanos()).unwrap_or(u64::MAX)
        };
        for (stage, (started, ended)) in [
            Stage::TerminalDiff,
            Stage::TerminalWrite,
            Stage::TerminalFlush,
        ]
        .into_iter()
        .zip(phases)
        {
            writer.record(
                Span {
                    stage,
                    start: nanos(started),
                    epoch: stamp.epoch,
                },
                nanos(ended),
            );
        }
        if let Some(input) = stamp.input {
            writer.record(input, nanos(ended_at));
        }
    });
}

pub(crate) fn input_consumed(event: &crate::tui::TuiEvent) -> Option<InputStamp> {
    let eligible = match event {
        crate::tui::TuiEvent::Key(_) | crate::tui::TuiEvent::Paste(_) => true,
        crate::tui::TuiEvent::Mouse(event) => matches!(
            event.kind,
            MouseEventKind::Down(_)
                | MouseEventKind::Up(_)
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        ),
        _ => return None,
    };
    WRITER
        .try_with(|writer| {
            let mut writer = writer.borrow_mut();
            writer.activate_reset();
            let now = writer.shared.now();
            writer.shared.last_input_ns.store(now, Ordering::Relaxed);
            eligible.then_some(InputStamp(Span {
                stage: Stage::ReactionLatency,
                start: now,
                epoch: writer.epoch,
            }))
        })
        .ok()
        .flatten()
}

pub(crate) fn input_handled(InputStamp(input): InputStamp) {
    let _ = WRITER.try_with(|writer| {
        let mut writer = writer.borrow_mut();
        writer.activate_reset();
        if input.epoch == writer.epoch {
            let pending = writer.ready_input.get_or_insert(input);
            if input.start < pending.start {
                *pending = input;
            }
        }
    });
}

const _: () = assert!(size_of::<Shared>() + size_of::<Writer>() <= 64 * 1024);

/// Actual coarse-boundary path for the external, Slurm-admitted cost harness.
/// Setup is outside the supplied measurement closure; no endpoint is started.
#[doc(hidden)]
pub async fn benchmark<R>(active: bool, measure: impl FnOnce(&dyn Fn(bool), usize) -> R) -> R {
    let boundary = |nested: bool| {
        let _outer = enter(Stage::Maintenance);
        let _ = std::hint::black_box(1_u64);
        if nested {
            let _child = enter(Stage::FramePreparation);
            let _ = std::hint::black_box(2_u64);
        }
    };
    if active {
        let shared = Arc::new(Shared::new());
        WRITER
            .scope(RefCell::new(Writer::new(shared)), async {
                measure(&boundary, size_of::<Shared>() + size_of::<Writer>())
            })
            .await
    } else {
        measure(&boundary, size_of::<Shared>() + size_of::<Writer>())
    }
}
