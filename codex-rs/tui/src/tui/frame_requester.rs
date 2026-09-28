//! Frame draw scheduling utilities for the TUI.
//!
//! This module exposes [`FrameRequester`], a lightweight handle that widgets and
//! background tasks can clone to request future redraws of the TUI.
//!
//! Internally it spawns a [`FrameScheduler`] task that coalesces many requests
//! into a single notification on a broadcast channel used by the main TUI event
//! loop. This keeps animations and status updates smooth without redrawing more
//! often than necessary.
//!
//! This follows the actor-style design from
//! [“Actors with Tokio”](https://ryhl.io/blog/actors-with-tokio/), with a
//! dedicated scheduler task and lightweight request handles.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use tokio::sync::broadcast;
use tokio::sync::mpsc;

use super::frame_rate_limiter::FrameRateLimiter;

const REDUCED_CONTENT_INTERVAL: Duration = Duration::from_millis(120);

fn scheduling_now() -> Instant {
    // Use Tokio's clock so elapsed deadlines and fake-clock scheduler tests share one epoch.
    tokio::time::Instant::now().into_std()
}

/// Admission of local presentation work; agent and server work is independent of this mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PresentationMode {
    #[default]
    Normal,
    Reduced,
    Frozen,
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum FrameKind {
    Content,
    Cosmetic,
    Maintenance,
    ModeChanged,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameRequest {
    pub(crate) at: Instant,
    kind: FrameKind,
    interactive: bool,
}

/// A requester for scheduling future frame draws on the TUI event loop.
///
/// This is the handler side of an actor/handler pair with `FrameScheduler`, which coalesces
/// multiple frame requests into a single draw operation.
///
/// Clones of this type can be freely shared across tasks to make it possible to trigger frame draws
/// from anywhere in the TUI code.
#[derive(Clone, Debug)]
pub struct FrameRequester {
    frame_schedule_tx: mpsc::UnboundedSender<FrameRequest>,
    mode: Arc<AtomicU8>,
    dirty: Arc<AtomicBool>,
}

impl FrameRequester {
    /// Create a new FrameRequester and spawn its associated FrameScheduler task.
    ///
    /// The provided `draw_tx` is used to notify the TUI event loop of scheduled draws.
    pub fn new(draw_tx: broadcast::Sender<()>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let mode = Arc::new(AtomicU8::new(PresentationMode::Normal as u8));
        let scheduler = FrameScheduler::new(rx, draw_tx, mode.clone());
        tokio::spawn(scheduler.run());
        Self {
            frame_schedule_tx: tx,
            mode,
            dirty: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Schedule a frame draw as soon as possible.
    pub fn schedule_frame(&self) {
        self.dirty.store(true, Ordering::Relaxed);
        if self.mode() != PresentationMode::Frozen {
            let _ = self.frame_schedule_tx.send(FrameRequest {
                at: scheduling_now(),
                kind: FrameKind::Content,
                interactive: false,
            });
        }
    }

    /// Admit meaningful local input without imposing the unattended stream cadence.
    pub(crate) fn schedule_interactive_frame(&self) {
        self.dirty.store(true, Ordering::Relaxed);
        if self.mode() != PresentationMode::Frozen {
            let _ = self.frame_schedule_tx.send(FrameRequest {
                at: scheduling_now(),
                kind: FrameKind::Content,
                interactive: true,
            });
        }
    }

    /// Schedule a frame draw to occur after the specified duration.
    pub fn schedule_frame_in(&self, dur: Duration) {
        if self.mode() == PresentationMode::Normal {
            let _ = self.frame_schedule_tx.send(FrameRequest {
                at: scheduling_now() + dur,
                kind: FrameKind::Cosmetic,
                interactive: false,
            });
        }
    }

    /// Reveal meaningful delayed content, unlike a cosmetic animation frame.
    pub(crate) fn schedule_content_in(&self, dur: Duration) {
        self.dirty.store(true, Ordering::Relaxed);
        if self.mode() != PresentationMode::Frozen {
            let _ = self.frame_schedule_tx.send(FrameRequest {
                at: scheduling_now() + dur,
                kind: FrameKind::Content,
                interactive: false,
            });
        }
    }

    /// Schedule due state work independently of cosmetic animation and painting.
    pub(crate) fn schedule_maintenance_in(&self, dur: Duration) {
        let _ = self.frame_schedule_tx.send(FrameRequest {
            at: scheduling_now() + dur,
            kind: FrameKind::Maintenance,
            interactive: false,
        });
    }

    pub(crate) fn mode(&self) -> PresentationMode {
        match self.mode.load(Ordering::Relaxed) {
            0 => PresentationMode::Normal,
            1 => PresentationMode::Reduced,
            2 => PresentationMode::Frozen,
            _ => unreachable!("presentation mode must be set through set_mode"),
        }
    }

    pub(crate) fn set_mode(&self, mode: PresentationMode) {
        let previous = self.mode.swap(mode as u8, Ordering::Relaxed);
        if previous == mode as u8 {
            return;
        }
        // Discard deadlines admitted in an earlier mode rather than waking for stale
        // animation while reduced or replaying a stale content deadline on thaw.
        let _ = self.frame_schedule_tx.send(FrameRequest {
            at: scheduling_now(),
            kind: FrameKind::ModeChanged,
            interactive: false,
        });
        if mode != PresentationMode::Frozen {
            // One fresh frame, never a replay of deadlines missed while frozen.
            self.schedule_interactive_frame();
        }
    }

    pub(crate) fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }
}

#[cfg(test)]
impl FrameRequester {
    /// Create a no-op frame requester for tests.
    pub(crate) fn test_dummy() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        FrameRequester {
            frame_schedule_tx: tx,
            mode: Arc::new(AtomicU8::new(PresentationMode::Normal as u8)),
            dirty: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// A scheduler for coalescing frame draw requests and notifying the TUI event loop.
///
/// This type is internal to `FrameRequester` and is spawned as a task to handle scheduling logic.
///
/// To avoid wasted redraw work, draw notifications are clamped to a maximum of 120 FPS (see
/// [`FrameRateLimiter`]).
struct FrameScheduler {
    receiver: mpsc::UnboundedReceiver<FrameRequest>,
    draw_tx: broadcast::Sender<()>,
    rate_limiter: FrameRateLimiter,
    mode: Arc<AtomicU8>,
}

impl FrameScheduler {
    /// Create a new FrameScheduler with the provided receiver and draw notification sender.
    fn new(
        receiver: mpsc::UnboundedReceiver<FrameRequest>,
        draw_tx: broadcast::Sender<()>,
        mode: Arc<AtomicU8>,
    ) -> Self {
        Self {
            receiver,
            draw_tx,
            rate_limiter: FrameRateLimiter::default(),
            mode,
        }
    }

    /// Run the scheduling loop, coalescing frame requests and notifying the TUI event loop.
    ///
    /// This method runs indefinitely until all senders are dropped. A single draw notification
    /// is sent for multiple requests scheduled before the next draw deadline.
    async fn run(mut self) {
        const ONE_YEAR: Duration = Duration::from_secs(60 * 60 * 24 * 365);
        let mut deadlines: [Option<Instant>; 3] = [None; 3];
        loop {
            let target = deadlines
                .iter()
                .flatten()
                .copied()
                .min()
                .unwrap_or_else(|| scheduling_now() + ONE_YEAR);
            let deadline = tokio::time::sleep_until(target.into());
            tokio::pin!(deadline);

            tokio::select! {
                request = self.receiver.recv() => {
                    let Some(request) = request else {
                        // All senders dropped; exit the scheduler.
                        break
                    };
                    self.absorb_request(&mut deadlines, request);
                    // Do not send a draw immediately here. By continuing the loop,
                    // we recompute the sleep target so the draw fires once via the
                    // sleep branch, coalescing multiple requests into a single draw.
                    continue;
                }
                _ = &mut deadline => {
                    // The timer can be ready alongside queued requests. Drain a bounded
                    // burst before emitting, rather than painting after the first request
                    // and then again for the rest of the same burst.
                    for _ in 0..64 {
                        let Ok(request) = self.receiver.try_recv() else {
                            break;
                        };
                        self.absorb_request(&mut deadlines, request);
                    }
                    let now = scheduling_now();
                    let due = deadlines.map(|deadline| deadline.is_some_and(|at| at <= now));
                    for (deadline, is_due) in deadlines.iter_mut().zip(due) {
                        if is_due {
                            *deadline = None;
                        }
                    }
                    let mode = self.mode.load(Ordering::Relaxed);
                    if due[FrameKind::Maintenance as usize]
                        || (mode != PresentationMode::Frozen as u8 && (
                            due[FrameKind::Content as usize]
                            || (mode == PresentationMode::Normal as u8
                                && due[FrameKind::Cosmetic as usize])
                        )) {
                            self.rate_limiter.mark_emitted(scheduling_now());
                            let _ = self.draw_tx.send(());
                    }
                }
            }
        }
    }

    fn absorb_request(&mut self, deadlines: &mut [Option<Instant>; 3], request: FrameRequest) {
        if matches!(request.kind, FrameKind::ModeChanged) {
            // Every transition starts a new presentation epoch. In particular,
            // Frozen may have been entered and left before this actor is polled.
            deadlines[FrameKind::Cosmetic as usize] = None;
            deadlines[FrameKind::Content as usize] = None;
            return;
        }
        if matches!(request.kind, FrameKind::Cosmetic)
            && self.mode.load(Ordering::Relaxed) != PresentationMode::Normal as u8
        {
            return;
        }
        let draw_at = if matches!(request.kind, FrameKind::Content)
            && self.mode.load(Ordering::Relaxed) == PresentationMode::Reduced as u8
            && !request.interactive
        {
            self.rate_limiter
                .clamp_deadline_with_interval(request.at, REDUCED_CONTENT_INTERVAL)
        } else {
            self.rate_limiter.clamp_deadline(request.at)
        };
        if matches!(request.kind, FrameKind::Content) {
            if deadlines[FrameKind::Cosmetic as usize]
                .is_some_and(|cosmetic| request.at <= cosmetic)
            {
                deadlines[FrameKind::Cosmetic as usize] = None;
            }
        } else if matches!(request.kind, FrameKind::Cosmetic)
            && deadlines[FrameKind::Content as usize].is_some_and(|content| content <= request.at)
        {
            return;
        }
        let deadline = &mut deadlines[request.kind as usize];
        *deadline = Some(deadline.map_or(draw_at, |cur| cur.min(draw_at)));
    }
}
#[cfg(test)]
mod tests {
    use super::super::frame_rate_limiter::MIN_FRAME_INTERVAL;
    use super::*;
    use tokio::time;
    use tokio_util::time::FutureExt;

    impl FrameRequester {
        /// Create a frame requester and expose its request channel for deterministic tests.
        pub(crate) fn test_channel() -> (Self, mpsc::UnboundedReceiver<FrameRequest>) {
            let (tx, rx) = mpsc::unbounded_channel();
            (
                FrameRequester {
                    frame_schedule_tx: tx,
                    mode: Arc::new(AtomicU8::new(PresentationMode::Normal as u8)),
                    dirty: Arc::new(AtomicBool::new(false)),
                },
                rx,
            )
        }
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_schedule_frame_immediate_triggers_once() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();

        // Advance time minimally to let the scheduler process and hit the deadline == now.
        time::advance(Duration::from_millis(1)).await;

        // First draw should arrive.
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for first draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        // No second draw should arrive.
        let second = draw_rx.recv().timeout(Duration::from_millis(20)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn frozen_frames_coalesce_until_return_but_maintenance_still_wakes() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);
        requester.schedule_frame_in(Duration::from_millis(/*millis*/ 100));
        tokio::task::yield_now().await;
        requester.set_mode(PresentationMode::Frozen);
        requester.schedule_frame();
        requester.schedule_frame_in(Duration::from_millis(/*millis*/ 1));
        tokio::task::yield_now().await;
        time::advance(Duration::from_millis(/*millis*/ 2)).await;
        assert!(draw_rx.try_recv().is_err());

        requester.schedule_maintenance_in(Duration::from_millis(/*millis*/ 10));
        assert!(
            matches!(
                draw_rx
                    .recv()
                    .timeout(Duration::from_millis(/*millis*/ 20))
                    .await,
                Ok(Ok(()))
            ),
            "maintenance must run while frozen"
        );
        assert!(draw_rx.try_recv().is_err());

        requester.set_mode(PresentationMode::Reduced);
        assert!(
            matches!(
                draw_rx
                    .recv()
                    .timeout(Duration::from_millis(/*millis*/ 20))
                    .await,
                Ok(Ok(()))
            ),
            "return paints current state once"
        );
        time::advance(Duration::from_millis(/*millis*/ 120)).await;
        assert!(draw_rx.try_recv().is_err(), "missed frames must not replay");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn reduced_content_coalesces_stream_bursts_without_delaying_state_work() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);
        requester.set_mode(PresentationMode::Reduced);
        tokio::task::yield_now().await;
        time::advance(Duration::from_millis(/*millis*/ 1)).await;
        assert!(draw_rx.try_recv().is_ok(), "mode transition renders once");

        for _ in 0..100 {
            requester.schedule_frame();
        }
        tokio::task::yield_now().await;
        time::advance(Duration::from_millis(/*millis*/ 40)).await;
        assert!(
            draw_rx.try_recv().is_err(),
            "burst must not repaint line by line"
        );

        requester.schedule_maintenance_in(Duration::from_millis(/*millis*/ 1));
        assert!(
            draw_rx
                .recv()
                .timeout(Duration::from_millis(/*millis*/ 20))
                .await
                .is_ok(),
            "maintenance has its own deadline"
        );
        assert!(
            matches!(
                draw_rx
                    .recv()
                    .timeout(REDUCED_CONTENT_INTERVAL + Duration::from_millis(/*millis*/ 20))
                    .await,
                Ok(Ok(()))
            ),
            "coalesced content eventually paints"
        );
        assert!(
            draw_rx.try_recv().is_err(),
            "burst produced only one content paint"
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn reduced_input_preempts_deferred_content_without_a_second_paint() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);
        requester.set_mode(PresentationMode::Reduced);
        tokio::task::yield_now().await;
        time::advance(Duration::from_millis(/*millis*/ 1)).await;
        assert!(draw_rx.try_recv().is_ok());
        requester.take_dirty();

        requester.schedule_frame();
        tokio::task::yield_now().await;
        time::advance(Duration::from_millis(/*millis*/ 30)).await;
        assert!(draw_rx.try_recv().is_err(), "backend content must coalesce");
        requester.schedule_interactive_frame();
        tokio::task::yield_now().await;
        time::advance(MIN_FRAME_INTERVAL + Duration::from_millis(/*millis*/ 1)).await;
        assert!(
            draw_rx.try_recv().is_ok(),
            "input must not wait on a stream deadline"
        );
        assert!(requester.take_dirty());

        time::advance(REDUCED_CONTENT_INTERVAL).await;
        assert!(
            draw_rx.try_recv().is_err(),
            "input already painted pending content"
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_schedule_frame_in_triggers_at_delay() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame_in(Duration::from_millis(50));

        // Advance less than the delay: no draw yet.
        time::advance(Duration::from_millis(30)).await;
        let early = draw_rx.recv().timeout(Duration::from_millis(10)).await;
        assert!(early.is_err(), "draw fired too early");

        // Advance past the deadline: one draw should fire.
        time::advance(Duration::from_millis(25)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for scheduled draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        // No second draw should arrive.
        let second = draw_rx.recv().timeout(Duration::from_millis(20)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_coalesces_multiple_requests_into_single_draw() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        // Schedule multiple immediate requests close together.
        requester.schedule_frame();
        requester.schedule_frame();
        requester.schedule_frame();

        // Allow the scheduler to process and hit the coalesced deadline.
        time::advance(Duration::from_millis(1)).await;

        // Expect only a single draw notification despite three requests.
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for coalesced draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        // No additional draw should be sent for the same coalesced batch.
        let second = draw_rx.recv().timeout(Duration::from_millis(20)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_coalesces_mixed_immediate_and_delayed_requests() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        // Schedule a delayed draw and then an immediate one; should coalesce and fire at the earliest (immediate).
        requester.schedule_frame_in(Duration::from_millis(100));
        requester.schedule_frame();

        time::advance(Duration::from_millis(1)).await;

        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for coalesced immediate draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        // The later delayed request should have been coalesced into the earlier one; no second draw.
        let second = draw_rx.recv().timeout(Duration::from_millis(120)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_limits_draw_notifications_to_120fps() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();
        time::advance(Duration::from_millis(1)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for first draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        requester.schedule_frame();
        time::advance(Duration::from_millis(1)).await;
        let early = draw_rx.recv().timeout(Duration::from_millis(1)).await;
        assert!(
            early.is_err(),
            "draw fired too early; expected max 120fps (min interval {MIN_FRAME_INTERVAL:?})"
        );

        time::advance(MIN_FRAME_INTERVAL).await;
        let second = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for second draw");
        assert!(second.is_ok(), "broadcast closed unexpectedly");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_late_draw_still_limits_the_next_frame() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);
        let delayed_deadline = Instant::now() - MIN_FRAME_INTERVAL.saturating_mul(2);
        requester
            .frame_schedule_tx
            .send(FrameRequest {
                at: delayed_deadline,
                kind: FrameKind::Content,
                interactive: false,
            })
            .expect("scheduler should accept the overdue draw");

        draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for overdue draw")
            .expect("broadcast closed unexpectedly");

        requester.schedule_frame();
        draw_rx
            .recv()
            .timeout(Duration::from_millis(1))
            .await
            .expect_err("a late draw must not trigger a redraw burst");

        time::advance(MIN_FRAME_INTERVAL).await;
        draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for rate-limited draw")
            .expect("broadcast closed unexpectedly");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_rate_limit_clamps_early_delayed_requests() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();
        time::advance(Duration::from_millis(1)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for first draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        requester.schedule_frame_in(Duration::from_millis(1));

        time::advance(MIN_FRAME_INTERVAL / 2).await;
        let too_early = draw_rx.recv().timeout(Duration::from_millis(1)).await;
        assert!(
            too_early.is_err(),
            "draw fired too early; expected max 120fps (min interval {MIN_FRAME_INTERVAL:?})"
        );

        time::advance(MIN_FRAME_INTERVAL).await;
        let second = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for clamped draw");
        assert!(second.is_ok(), "broadcast closed unexpectedly");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_rate_limit_does_not_delay_future_draws() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();
        time::advance(Duration::from_millis(1)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for first draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        requester.schedule_frame_in(Duration::from_millis(50));

        time::advance(Duration::from_millis(49)).await;
        let early = draw_rx.recv().timeout(Duration::from_millis(1)).await;
        assert!(early.is_err(), "draw fired too early");

        time::advance(Duration::from_millis(1)).await;
        let second = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for delayed draw");
        assert!(second.is_ok(), "broadcast closed unexpectedly");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_multiple_delayed_requests_coalesce_to_earliest() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        // Schedule multiple delayed draws; they should coalesce to the earliest (10ms).
        requester.schedule_frame_in(Duration::from_millis(100));
        requester.schedule_frame_in(Duration::from_millis(20));
        requester.schedule_frame_in(Duration::from_millis(120));

        // Advance to just before the earliest deadline: no draw yet.
        time::advance(Duration::from_millis(10)).await;
        let early = draw_rx.recv().timeout(Duration::from_millis(10)).await;
        assert!(early.is_err(), "draw fired too early");

        // Advance past the earliest deadline: one draw should fire.
        time::advance(Duration::from_millis(20)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for earliest coalesced draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        // No additional draw should fire for the later delayed requests.
        let second = draw_rx.recv().timeout(Duration::from_millis(120)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }
}
