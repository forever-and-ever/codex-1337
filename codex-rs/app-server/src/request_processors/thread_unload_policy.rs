//! Timed close follows core custody; the independent cleanup owner fences attachment.
use super::ListenerTaskContext;
use crate::thread_state::SubscriberActivity;
use crate::thread_state::ThreadState;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadClosedNotification;
use codex_app_server_protocol::ThreadStatus;
use codex_core::CodexThread;
use codex_core::ThreadEvictionOutcome;
use codex_core::runtime_holds::RuntimeActivity;
use codex_protocol::ThreadId;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tracing::warn;

const CONTENTION_BACKOFF: Duration = Duration::from_secs(1);

enum Wake {
    Deadline,
    Activity,
    ContendedUntil(Instant),
}
enum Observation {
    Live {
        activity: watch::Receiver<RuntimeActivity>,
        status: watch::Receiver<ThreadStatus>,
    },
    Relay,
}
pub(super) struct UnloadingState {
    delay: Duration,
    subscribers: watch::Receiver<SubscriberActivity>,
    runtime: Observation,
    wake: Wake,
}
impl UnloadingState {
    pub(super) fn new(
        delay: Duration,
        subscribers: watch::Receiver<SubscriberActivity>,
        activity: watch::Receiver<RuntimeActivity>,
        status: watch::Receiver<ThreadStatus>,
    ) -> Self {
        Self {
            delay,
            subscribers,
            runtime: Observation::Live { activity, status },
            wake: Wake::Deadline,
        }
    }
    fn target(&self) -> Option<Instant> {
        let SubscriberActivity::Detached { since } = *self.subscribers.borrow() else {
            return None;
        };
        let quiet = match &self.runtime {
            Observation::Live { activity, .. } => match *activity.borrow() {
                RuntimeActivity::Quiet { since, .. } => since,
                RuntimeActivity::Held { .. } | RuntimeActivity::Closing => return None,
            },
            Observation::Relay => since,
        };
        since.max(quiet).checked_add(self.delay)
    }
    pub(super) fn should_unload_now(&self) -> bool {
        self.target().is_some_and(|target| target <= Instant::now())
    }
    pub(super) fn defer_until_activity(&mut self) {
        self.wake = Wake::Activity;
    }
    pub(super) fn defer_contention(&mut self) {
        self.wake = Wake::ContendedUntil(Instant::now() + CONTENTION_BACKOFF);
    }
    pub(super) fn observe_relay(&mut self) {
        self.runtime = Observation::Relay;
        self.wake = Wake::Deadline;
    }
    pub(super) async fn wait_for_unloading_trigger(&mut self) -> bool {
        loop {
            let target = match self.wake {
                Wake::Deadline => self.target(),
                Wake::Activity => None,
                Wake::ContendedUntil(retry) => self.target().map(|target| target.max(retry)),
            };
            let timer = async {
                if let Some(target) = target {
                    tokio::time::sleep_until(target.into()).await;
                } else {
                    futures::future::pending::<()>().await;
                }
            };
            tokio::select! {
                () = timer => return true,
                changed = self.subscribers.changed() => { if changed.is_err() { return false; } }
                changed = async {
                    match &mut self.runtime {
                        Observation::Live { activity, status } => tokio::select! {
                            changed = activity.changed() => changed,
                            changed = status.changed() => changed,
                        },
                        Observation::Relay => futures::future::pending().await,
                    }
                } => { if changed.is_err() { return false; } }
            }
            self.wake = Wake::Deadline;
        }
    }
}

pub(super) async fn begin_idle_unload(
    context: &ListenerTaskContext,
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    state: Arc<Mutex<ThreadState>>,
    generation: u64,
    delay: Duration,
    resume_guard: tokio::sync::OwnedMutexGuard<()>,
) -> Result<oneshot::Receiver<ThreadEvictionOutcome>, ThreadEvictionOutcome> {
    let eligibility = thread
        .idle_close_eligibility(delay)
        .ok_or(ThreadEvictionOutcome::Busy)?;
    let mut pending = context.pending_thread_unloads.lock().await;
    if pending.contains_key(&thread_id) {
        return Err(ThreadEvictionOutcome::Busy);
    }
    match context
        .thread_state_manager
        .try_subscriber_activity(thread_id)
    {
        Ok(SubscriberActivity::Detached { .. }) => {}
        Ok(SubscriberActivity::Attached) => return Err(ThreadEvictionOutcome::Busy),
        Err(_) => return Err(ThreadEvictionOutcome::Contended),
    }
    let marker = watch::channel(()).0;
    pending.insert(thread_id, marker.clone());
    let context = context.clone();
    let (result_tx, result_rx) = oneshot::channel();
    // Publication and transfer have no intervening cancellation point.
    tokio::spawn(async move {
        let _resume_guard = resume_guard;
        let outcome = match context
            .thread_manager
            .try_idle_close(thread, eligibility)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                warn!("failed to idle-close thread {thread_id}: {error}");
                ThreadEvictionOutcome::Busy
            }
        };
        if matches!(
            outcome,
            ThreadEvictionOutcome::Evicted | ThreadEvictionOutcome::NotCurrent
        ) {
            let removed = context
                .thread_state_manager
                .remove_unsubscribed_listener(thread_id, &state, generation)
                .await;
            if removed && outcome == ThreadEvictionOutcome::Evicted {
                context
                    .outgoing
                    .cancel_requests_for_thread(thread_id, None)
                    .await;
                context
                    .thread_watch_manager
                    .remove_thread(&thread_id.to_string())
                    .await;
                context
                    .outgoing
                    .send_server_notification(ServerNotification::ThreadClosed(
                        ThreadClosedNotification {
                            thread_id: thread_id.to_string(),
                        },
                    ))
                    .await;
            }
        }
        let mut pending = context.pending_thread_unloads.lock().await;
        if pending
            .get(&thread_id)
            .is_some_and(|current| current.same_channel(&marker))
        {
            pending.remove(&thread_id);
        }
        let _ = result_tx.send(outcome);
    });
    drop(pending);
    Ok(result_rx)
}
