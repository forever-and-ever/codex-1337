//! Prompt-cache estimates and warming holds. Process-local: they survive runtime unload, never a
//! server restart.
//!
//! Warming is a standing intent with a deadline. A human message or the toggle itself restarts
//! the hold; any lapse turns warming off, so a later one-off follow-up never revives it.
#![expect(
    clippy::expect_used,
    reason = "Registry poisoning is a fatal coordinator invariant violation."
)]
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use codex_app_server_protocol::PromptCacheEstimate;
use codex_app_server_protocol::PromptCacheWarming;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadPromptCacheMissedNotification;
use codex_app_server_protocol::ThreadPromptCacheUpdatedNotification;
use codex_app_server_protocol::ThreadSetWarmingResponse;
use codex_app_server_protocol::ThreadWarmingStoppedNotification;
use codex_core::CodexThread;
use codex_core::CompletedRequest;
use codex_core::PromptCacheFact;
use codex_core::PromptCacheObserver;
use codex_core::RequestStart;
use codex_core::Unreplayable;
use codex_core::WarmingLore;
use codex_core::WarmingRequest;
use codex_protocol::ThreadId;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::openai_models::AnthropicCacheTtl;
use codex_protocol::protocol::TokenUsage;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::SystemTime;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// A can-warm event keeps the cache warm this long.
const HOLD: Duration = Duration::from_hours(/*secs*/ 8);
/// Replays land this long before the cache would expire.
const LEAD: Duration = Duration::from_mins(/*secs*/ 3);
const MAX_REPLAYS: usize = 64;
const MAX_REPLAY_BYTES: usize = 128_000_000;

/// Cheap handle onto the server's prompt-cache registry.
#[derive(Clone)]
pub(crate) struct PromptCache(Arc<Shared>);

/// Owns the scheduler and publisher; both stop with the server.
pub(crate) struct PromptCacheWorker([JoinHandle<()>; 2]);

impl Drop for PromptCacheWorker {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

struct Shared {
    registry: Mutex<HashMap<ThreadId, Entry>>,
    changed: Notify,
    publish: mpsc::UnboundedSender<Publication>,
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptCache").finish_non_exhaustive()
    }
}

/// Publications keep one order, so a targeted snapshot never overtakes a later broadcast.
enum Publication {
    Broadcast(ServerNotification),
    To(ConnectionId, ServerNotification),
}

#[derive(Default)]
struct Entry {
    /// Invalidations seen; a completion quoting an older epoch describes a stale prefix.
    epoch: u64,
    cache: Cache,
    warming: Warming,
}

#[derive(Default)]
enum Cache {
    /// No current evidence of retained cache warmth.
    #[default]
    Cold,
    /// The prefix was last read or written at refreshed; replay keeps it warm when retained.
    Warm {
        refreshed: RequestStart,
        lore: WarmingLore,
        replay: Option<Arc<WarmingRequest>>,
    },
}

#[derive(Default, Clone, Copy)]
enum Warming {
    #[default]
    Off,
    On {
        until: SystemTime,
    },
}

impl Entry {
    fn view(&self) -> (PromptCacheEstimate, PromptCacheWarming) {
        let estimate = match &self.cache {
            Cache::Warm {
                refreshed, lore, ..
            } if refreshed.age() < lore.lifetime() => PromptCacheEstimate::Warm {
                expires_at: unix_seconds(refreshed.wall() + lore.lifetime()),
            },
            Cache::Warm { .. } | Cache::Cold => PromptCacheEstimate::Cold,
        };
        let warming = match self.warming {
            Warming::Off => PromptCacheWarming::Off,
            Warming::On { until } if self.retains_replay() => PromptCacheWarming::Holding {
                ends_at: unix_seconds(until),
            },
            Warming::On { until } => PromptCacheWarming::Armed {
                ends_at: unix_seconds(until),
            },
        };
        (estimate, warming)
    }

    fn retains_replay(&self) -> bool {
        matches!(
            self.cache,
            Cache::Warm {
                replay: Some(_),
                ..
            }
        )
    }

    /// The retained replay, while the prefix it keeps warm has not expired.
    fn live_replay(&self) -> Option<(&Arc<WarmingRequest>, RequestStart, Duration)> {
        match &self.cache {
            Cache::Warm {
                refreshed,
                lore,
                replay: Some(replay),
            } if refreshed.age() < lore.lifetime() => Some((replay, *refreshed, lore.lifetime())),
            Cache::Warm { .. } | Cache::Cold => None,
        }
    }

    fn expected_hit(&self, started: RequestStart) -> bool {
        matches!(&self.cache, Cache::Warm { refreshed, lore, .. }
            if refreshed.wall() + lore.lifetime() > started.wall())
    }

    /// Drops an expired replay, since an expired prefix stays expired until real work rewrites
    /// it, and ends warming that lapsed.
    fn lapse(&mut self, thread: ThreadId, now: SystemTime) -> Option<ServerNotification> {
        if let Cache::Warm {
            refreshed,
            lore,
            replay: replay @ Some(_),
        } = &mut self.cache
            && refreshed.age() >= lore.lifetime()
        {
            *replay = None;
            return self.stop(thread, "The cache expired before it could be replayed.");
        }
        match self.warming {
            Warming::On { until } if now >= until => {
                self.stop(thread, "No message in eight hours.")
            }
            Warming::On { .. } | Warming::Off => None,
        }
    }

    fn stop(&mut self, thread: ThreadId, reason: impl Into<String>) -> Option<ServerNotification> {
        matches!(self.warming, Warming::On { .. }).then(|| {
            self.warming = Warming::Off;
            ServerNotification::ThreadWarmingStopped(ThreadWarmingStoppedNotification {
                thread_id: thread.to_string(),
                reason: reason.into(),
            })
        })
    }
}

/// Why warming cannot hold this conversation, if it cannot.
fn refusal(lore: Option<WarmingLore>, replay: Option<Unreplayable>) -> Option<&'static str> {
    match (lore, replay) {
        (_, Some(Unreplayable::Subagent)) => Some("Subagents are not warmed."),
        (None | Some(WarmingLore::OpenAi), _) | (_, Some(Unreplayable::Provider)) => {
            Some("Only Claude conversations can be warmed.")
        }
        (
            Some(WarmingLore::Claude {
                ttl: AnthropicCacheTtl::FiveMinutes,
            }),
            _,
        ) => {
            Some("This conversation's cache lives five minutes; only hour-long caches are warmed.")
        }
        (_, Some(Unreplayable::RequestShape)) => Some(
            "Requests with manual thinking, structured output or forced tools cannot be replayed without output.",
        ),
        (_, Some(Unreplayable::Uncached)) => {
            Some("Nothing was cached; the conversation is too short.")
        }
        (
            Some(WarmingLore::Claude {
                ttl: AnthropicCacheTtl::OneHour,
            }),
            None,
        ) => None,
    }
}

impl PromptCache {
    pub(crate) fn spawn(outgoing: Arc<OutgoingMessageSender>) -> (Self, PromptCacheWorker) {
        let (publish, mut publications) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            registry: Mutex::default(),
            changed: Notify::new(),
            publish,
        });
        let publisher = tokio::spawn(async move {
            while let Some(publication) = publications.recv().await {
                match publication {
                    Publication::Broadcast(notification) => {
                        outgoing.send_server_notification(notification).await;
                    }
                    Publication::To(connection, notification) => {
                        outgoing
                            .send_server_notification_to_connections(&[connection], notification)
                            .await;
                    }
                }
            }
        });
        let scheduler = tokio::spawn(Arc::clone(&shared).run());
        (Self(shared), PromptCacheWorker([scheduler, publisher]))
    }

    pub(crate) fn observer(&self) -> Arc<dyn PromptCacheObserver> {
        self.0.clone()
    }

    /// Enabling counts as a can-warm event; it never heats a cold cache itself.
    pub(crate) async fn set_warming(
        &self,
        thread_id: ThreadId,
        thread: &CodexThread,
        enabled: bool,
    ) -> Result<ThreadSetWarmingResponse, &'static str> {
        if enabled {
            let subagent = thread
                .config_snapshot()
                .await
                .session_source
                .is_non_root_agent();
            let lore = thread.prompt_cache_lore().await;
            if let Some(reason) = refusal(lore, subagent.then_some(Unreplayable::Subagent)) {
                return Err(reason);
            }
        }
        let (estimate, warming) = self.0.update(thread_id, |entry| {
            entry.warming = if enabled {
                Warming::On {
                    until: SystemTime::now() + HOLD,
                }
            } else {
                Warming::Off
            };
            Vec::new()
        });
        Ok(ThreadSetWarmingResponse { estimate, warming })
    }

    /// Publishes observed cache state; thread metadata cannot reconstruct a request lease.
    pub(crate) fn publish_to(&self, connection: ConnectionId, thread_id: ThreadId) {
        let mut registry = self.0.lock();
        let entry = registry.entry(thread_id).or_default();
        self.0.send(Publication::To(
            connection,
            state_notification(thread_id, entry.view()),
        ));
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ThreadId, Entry>> {
        self.registry
            .lock()
            .expect("prompt-cache registry poisoned")
    }

    fn send(&self, publication: Publication) {
        // The publisher stops only with the server.
        let _ = self.publish.send(publication);
    }

    /// Applies a change, then publishes its events and any visible state change, in order.
    fn update(
        &self,
        thread: ThreadId,
        change: impl FnOnce(&mut Entry) -> Vec<ServerNotification>,
    ) -> (PromptCacheEstimate, PromptCacheWarming) {
        let mut registry = self.lock();
        let entry = registry.entry(thread).or_default();
        let before = entry.view();
        let events = change(entry);
        let after = entry.view();
        for event in events {
            self.send(Publication::Broadcast(event));
        }
        if after != before {
            self.send(Publication::Broadcast(state_notification(thread, after)));
        }
        trim_retention(&mut registry);
        self.changed.notify_one();
        after
    }

    async fn run(self: Arc<Self>) {
        loop {
            match self.next() {
                Next::Replay(thread, replay) => {
                    let result = replay.send(|| self.wanted(thread, &replay)).await;
                    self.replayed(thread, &replay, result);
                }
                Next::Wait(None) => self.changed.notified().await,
                Next::Wait(Some(delay)) => {
                    tokio::select! {
                        () = self.changed.notified() => {}
                        () = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }
    }

    /// Lapses expired replays and holds, and finds the most overdue replay or how long to wait.
    fn next(&self) -> Next {
        let now = SystemTime::now();
        let mut registry = self.lock();
        let mut wait: Option<Duration> = None;
        let mut due: Option<(ThreadId, Arc<WarmingRequest>, Duration)> = None;
        for (thread, entry) in registry.iter_mut() {
            let before = entry.view();
            let stopped = entry.lapse(*thread, now);
            if let Warming::On { until } = entry.warming {
                if let Some((replay, refreshed, lifetime)) = entry.live_replay() {
                    let age = refreshed.age();
                    let replay_in = lifetime.saturating_sub(LEAD).saturating_sub(age);
                    if replay_in.is_zero() {
                        if due.as_ref().is_none_or(|(_, _, overdue)| age > *overdue) {
                            due = Some((*thread, Arc::clone(replay), age));
                        }
                    } else {
                        wait = Some(wait.map_or(replay_in, |wait| wait.min(replay_in)));
                    }
                }
                let until_in = until.duration_since(now).unwrap_or_default();
                wait = Some(wait.map_or(until_in, |wait| wait.min(until_in)));
            }
            if let Some(stopped) = stopped {
                self.send(Publication::Broadcast(stopped));
            }
            let after = entry.view();
            if after != before {
                self.send(Publication::Broadcast(state_notification(*thread, after)));
            }
        }
        // Wall-clock jumps and suspend do not advance the monotonic timer.
        let wait = wait.map(|wait| wait.min(Duration::from_secs(/*secs*/ 60)));
        match due {
            Some((thread, replay, _)) => Next::Replay(thread, replay),
            None => Next::Wait(wait),
        }
    }

    fn wanted(&self, thread: ThreadId, replay: &Arc<WarmingRequest>) -> bool {
        self.lock().get(&thread).is_some_and(|entry| {
            matches!(entry.warming, Warming::On { until } if SystemTime::now() < until)
                && entry
                    .live_replay()
                    .is_some_and(|(retained, _, _)| Arc::ptr_eq(retained, replay))
        })
    }

    fn replayed(
        &self,
        thread: ThreadId,
        replay: &Arc<WarmingRequest>,
        result: CodexResult<Option<(RequestStart, TokenUsage)>>,
    ) {
        let result = match result {
            Ok(None) => return,
            Ok(Some(sent)) => Ok(sent),
            Err(error) => Err(error),
        };
        let _ = self.update(thread, |entry| {
            let Cache::Warm {
                refreshed,
                replay: Some(retained),
                ..
            } = &mut entry.cache
            else {
                return Vec::new();
            };
            if !Arc::ptr_eq(retained, replay) {
                return Vec::new();
            }
            match result {
                Ok((started, usage)) => {
                    tracing::debug!(
                        %thread,
                        cache_read = usage.cached_input_tokens,
                        cache_write = usage.cache_write_input_tokens,
                        "prompt-cache warm replay"
                    );
                    if usage.cached_input_tokens > 0 || usage.cache_write_input_tokens > 0 {
                        *refreshed = started;
                    } else {
                        entry.cache = Cache::Cold;
                    }
                    if usage.cache_write_input_tokens == 0 && usage.cached_input_tokens > 0 {
                        return Vec::new();
                    }
                    // Never rebuild: the replay already rewrote the prefix once.
                    let missed = ServerNotification::ThreadPromptCacheMissed(
                        ThreadPromptCacheMissedNotification {
                            thread_id: thread.to_string(),
                            rewritten_tokens: usage.cache_write_input_tokens,
                            while_warming: true,
                        },
                    );
                    entry.warming = Warming::Off;
                    vec![missed]
                }
                Err(error) => entry
                    .stop(thread, format!("The warming replay failed: {error}"))
                    .into_iter()
                    .collect(),
            }
        });
    }
}

enum Next {
    Replay(ThreadId, Arc<WarmingRequest>),
    Wait(Option<Duration>),
}

impl PromptCacheObserver for Shared {
    fn epoch(&self, thread: ThreadId) -> u64 {
        self.lock().get(&thread).map_or(0, |entry| entry.epoch)
    }

    fn fact(&self, thread: ThreadId, fact: PromptCacheFact) {
        let _ = self.update(thread, |entry| {
            match fact {
                PromptCacheFact::HumanTurn => {
                    if let Warming::On { until } = &mut entry.warming {
                        *until = SystemTime::now() + HOLD;
                    }
                }
                PromptCacheFact::Invalidated => {
                    entry.epoch += 1;
                    entry.cache = Cache::Cold;
                }
            }
            Vec::new()
        });
    }

    fn completed(&self, thread: ThreadId, request: CompletedRequest) {
        let CompletedRequest {
            epoch,
            started,
            lore,
            usage,
            replay,
        } = request;
        let _ = self.update(thread, |entry| {
            if epoch != entry.epoch {
                return Vec::new();
            }
            let mut events = Vec::new();
            if entry.expected_hit(started)
                && usage.cached_input_tokens == 0
                && usage.cache_write_input_tokens > 0
            {
                events.push(ServerNotification::ThreadPromptCacheMissed(
                    ThreadPromptCacheMissedNotification {
                        thread_id: thread.to_string(),
                        rewritten_tokens: usage.cache_write_input_tokens,
                        while_warming: false,
                    },
                ));
            }
            if let Some(reason) = refusal(Some(lore), replay.as_ref().err().copied()) {
                events.extend(entry.stop(thread, reason));
            }
            entry.cache = if usage.cached_input_tokens > 0 || usage.cache_write_input_tokens > 0 {
                Cache::Warm {
                    refreshed: started,
                    lore,
                    replay: replay.ok().map(Arc::new),
                }
            } else {
                Cache::Cold
            };
            events
        });
    }
}

/// Bounds retained replays by dropping those of conversations that are not warming, oldest
/// first. Idle conversations retain one only so enabling warming can hold a warm cache at once.
fn trim_retention(registry: &mut HashMap<ThreadId, Entry>) {
    let mut count = 0;
    let mut bytes = 0;
    let mut idle = Vec::new();
    for entry in registry.values_mut() {
        let warming = matches!(entry.warming, Warming::On { .. });
        if let Cache::Warm {
            refreshed,
            replay: replay @ Some(_),
            ..
        } = &mut entry.cache
        {
            let size = replay.as_ref().map_or(0, |replay| replay.retained_bytes());
            count += 1;
            bytes += size;
            if !warming {
                idle.push((refreshed.age(), size, replay));
            }
        }
    }
    idle.sort_by_key(|(age, _, _)| std::cmp::Reverse(*age));
    for (_, size, replay) in idle {
        if count <= MAX_REPLAYS && bytes <= MAX_REPLAY_BYTES {
            break;
        }
        *replay = None;
        count -= 1;
        bytes -= size;
    }
}

fn state_notification(
    thread: ThreadId,
    (estimate, warming): (PromptCacheEstimate, PromptCacheWarming),
) -> ServerNotification {
    ServerNotification::ThreadPromptCacheUpdated(ThreadPromptCacheUpdatedNotification {
        thread_id: thread.to_string(),
        estimate,
        warming,
    })
}

fn unix_seconds(at: SystemTime) -> i64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}
