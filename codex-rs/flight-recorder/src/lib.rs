//! Per-process flight recorder: a byte-bounded, tiered in-memory ring of tracing events.
//!
//! Routine diagnostics never reach disk. They leave the process only through an explicit query
//! (the app-server's `debug/flightRecorder/read`), an on-demand `SIGUSR1` dump, or the panic
//! hook. Each [`Tier`] has its own byte budget, so DEBUG/TRACE chatter can never evict INFO and
//! INFO can never evict WARN/ERROR.
//!
//! The ring lock is held only to append, evict or copy records. Nothing under it emits tracing
//! events (the layer would re-enter the lock), awaits, or performs I/O, with one deliberate
//! exception: the panic hook writes its dump while holding the lock, streaming records through
//! a fixed buffer, so a panicking process never allocates a copy of the ring.

mod dump;
mod layer;
mod render;

pub use dump::Process;
pub use dump::dump_dir;
pub use dump::install_panic_hook;
#[cfg(unix)]
pub use dump::spawn_signal_dump;
pub use render::render_record;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::sync::TryLockError;

use tracing::Level;

/// Per-record bookkeeping charged against a tier's budget in addition to its text.
const RECORD_OVERHEAD_BYTES: usize = 64;

static GLOBAL: OnceLock<FlightRecorder> = OnceLock::new();

/// Byte budgets for the three tiers; their sum bounds the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    pub alert: usize,
    pub info: usize,
    pub chatter: usize,
}

impl Budgets {
    /// The daemon multiplexes every thread, so it keeps the longest horizon: 16 MiB.
    pub const SERVER: Self = Self {
        alert: 1 << 20,
        info: 5 << 20,
        chatter: 10 << 20,
    };
    /// Interactive and headless clients log far less than the daemon: 4 MiB.
    pub const CLIENT: Self = Self {
        alert: 512 << 10,
        info: 1536 << 10,
        chatter: 2 << 20,
    };

    fn of(self, tier: Tier) -> usize {
        match tier {
            Tier::Alert => self.alert,
            Tier::Info => self.info,
            Tier::Chatter => self.chatter,
        }
    }
}

/// Retention class of a record; each tier evicts only its own records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// WARN and ERROR.
    Alert,
    /// INFO.
    Info,
    /// DEBUG and TRACE.
    Chatter,
}

impl Tier {
    pub const ALL: [Self; 3] = [Self::Alert, Self::Info, Self::Chatter];

    pub fn of(level: Level) -> Self {
        match level {
            Level::ERROR | Level::WARN => Self::Alert,
            Level::INFO => Self::Info,
            _ => Self::Chatter,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Alert => 0,
            Self::Info => 1,
            Self::Chatter => 2,
        }
    }
}

/// Severity rank: larger is more severe. `tracing::Level` orders by verbosity instead.
pub fn severity(level: Level) -> u8 {
    match level {
        Level::ERROR => 4,
        Level::WARN => 3,
        Level::INFO => 2,
        Level::DEBUG => 1,
        Level::TRACE => 0,
    }
}

/// One captured event. `text` carries the span chain, message and fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Process-wide capture order; ties in `unix_ms` resolve by it.
    pub seq: u64,
    pub unix_ms: i64,
    pub level: Level,
    pub target: &'static str,
    pub thread_id: Option<Box<str>>,
    pub text: Box<str>,
}

impl Record {
    fn cost(&self) -> usize {
        RECORD_OVERHEAD_BYTES + self.text.len() + self.thread_id.as_deref().map_or(0, str::len)
    }
}

/// Selects records; every present criterion must hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordFilter {
    pub thread_id: Option<String>,
    /// Least severe level admitted.
    pub min_level: Option<Level>,
    pub target_prefix: Option<String>,
    pub since_unix_ms: Option<i64>,
    pub until_unix_ms: Option<i64>,
    /// Only records pushed after this sequence number.
    pub after_seq: Option<u64>,
    /// Keep only the newest this many matches.
    pub limit: Option<usize>,
}

impl RecordFilter {
    fn admits(&self, record: &Record) -> bool {
        self.thread_id
            .as_deref()
            .is_none_or(|thread_id| record.thread_id.as_deref() == Some(thread_id))
            && self
                .min_level
                .is_none_or(|level| severity(record.level) >= severity(level))
            && self
                .target_prefix
                .as_deref()
                .is_none_or(|prefix| record.target.starts_with(prefix))
            && self
                .since_unix_ms
                .is_none_or(|since| record.unix_ms >= since)
            && self
                .until_unix_ms
                .is_none_or(|until| record.unix_ms <= until)
            && self.after_seq.is_none_or(|seq| record.seq > seq)
    }
}

/// Occupancy of one tier when an excerpt was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TierUsage {
    pub tier: Tier,
    pub budget_bytes: usize,
    pub used_bytes: usize,
    /// The tier's horizon: nothing older survives in it.
    pub oldest_unix_ms: Option<i64>,
}

/// Matching records in capture order, plus the occupancy that bounds what could match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excerpt {
    pub records: Vec<Record>,
    pub tiers: [TierUsage; 3],
}

struct Lane {
    budget: usize,
    used: usize,
    records: VecDeque<Record>,
}

impl Lane {
    fn push(&mut self, record: Record) {
        let cost = record.cost();
        while self.used + cost > self.budget {
            let Some(evicted) = self.records.pop_front() else {
                break;
            };
            self.used -= evicted.cost();
        }
        if cost <= self.budget {
            self.used += cost;
            self.records.push_back(record);
        }
    }
}

pub(crate) struct Ring {
    lanes: [Lane; 3],
    next_seq: u64,
}

impl Ring {
    /// Visits every record in capture order without allocating.
    pub(crate) fn for_each_in_order(&self, mut visit: impl FnMut(&Record)) {
        let mut cursors = [0_usize; 3];
        loop {
            let next = (0..3)
                .filter_map(|lane| {
                    self.lanes[lane]
                        .records
                        .get(cursors[lane])
                        .map(|record| (lane, record))
                })
                .min_by_key(|(_, record)| record.seq);
            let Some((lane, record)) = next else {
                return;
            };
            visit(record);
            cursors[lane] += 1;
        }
    }
}

/// Handle to a process's ring. Cloning shares the ring.
#[derive(Clone)]
pub struct FlightRecorder {
    ring: Arc<Mutex<Ring>>,
}

impl FlightRecorder {
    pub fn new(budgets: Budgets) -> Self {
        let lane = |tier| Lane {
            budget: budgets.of(tier),
            used: 0,
            records: VecDeque::new(),
        };
        Self {
            ring: Arc::new(Mutex::new(Ring {
                lanes: Tier::ALL.map(lane),
                next_seq: 0,
            })),
        }
    }

    /// Installs the process recorder once; later calls return the first one unchanged.
    pub fn install(budgets: Budgets) -> &'static Self {
        GLOBAL.get_or_init(|| Self::new(budgets))
    }

    pub fn global() -> Option<&'static Self> {
        GLOBAL.get()
    }

    /// Captures events into this ring, excluding payload-bearing targets.
    pub fn layer<S>(&self) -> impl tracing_subscriber::Layer<S> + Send + Sync + 'static
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        layer::filtered(self.clone())
    }

    fn lock(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Never blocks: a panic raised while this thread holds the lock must not deadlock.
    pub(crate) fn try_lock(&self) -> Option<MutexGuard<'_, Ring>> {
        match self.ring.try_lock() {
            Ok(ring) => Some(ring),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    pub(crate) fn push(
        &self,
        unix_ms: i64,
        level: Level,
        target: &'static str,
        thread_id: Option<Box<str>>,
        text: Box<str>,
    ) {
        let mut ring = self.lock();
        let seq = ring.next_seq;
        ring.next_seq += 1;
        ring.lanes[Tier::of(level).index()].push(Record {
            seq,
            unix_ms,
            level,
            target,
            thread_id,
            text,
        });
    }

    /// Copies the newest matching records, oldest first.
    pub fn excerpt(&self, filter: &RecordFilter) -> Excerpt {
        let limit = filter.limit.unwrap_or(usize::MAX);
        let (mut records, tiers) = {
            let ring = self.lock();
            let mut records = Vec::new();
            for lane in &ring.lanes {
                // Each lane is in capture order, so its newest matches suffice for the merge.
                records.extend(
                    lane.records
                        .iter()
                        .rev()
                        .filter(|record| filter.admits(record))
                        .take(limit)
                        .cloned(),
                );
            }
            let tiers = Tier::ALL.map(|tier| {
                let lane = &ring.lanes[tier.index()];
                TierUsage {
                    tier,
                    budget_bytes: lane.budget,
                    used_bytes: lane.used,
                    oldest_unix_ms: lane.records.front().map(|record| record.unix_ms),
                }
            });
            (records, tiers)
        };
        records.sort_unstable_by_key(|record| record.seq);
        let excess = records.len().saturating_sub(limit);
        records.drain(..excess);
        Excerpt { records, tiers }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
