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

const UNFOCUSED_CONTENT_INTERVAL: Duration = Duration::from_millis(120);

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
    /// Visible but unfocused: content coalesces, and only the status row moves, glinting on a
    /// slower beat.
    Peripheral,
    Reduced,
    Frozen,
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum FrameKind {
    Content,
    /// Animation, on the beat; only a focused window admits it.
    Cosmetic,
    /// The status row's motion alone, painted between full frames.
    Glint,
    Maintenance,
    ModeChanged,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameRequest {
    pub(crate) at: Instant,
    kind: FrameKind,
    interactive: bool,
}

#[path = "background_preparation.rs"]
mod background_preparation;
mod beat;
mod paint_schedule;
pub(crate) use beat::Due;

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
    due: Arc<AtomicU8>,
    paint_admitted: Arc<AtomicBool>,
    background_preparation: Arc<AtomicBool>,
}

impl FrameRequester {
    /// Create a new FrameRequester and spawn its associated FrameScheduler task.
    ///
    /// The provided `draw_tx` is used to notify the TUI event loop of scheduled draws.
    pub fn new(draw_tx: broadcast::Sender<()>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let mode = Arc::new(AtomicU8::new(PresentationMode::Normal as u8));
        let due = Arc::new(AtomicU8::new(0));
        let scheduler = FrameScheduler::new(rx, draw_tx, mode.clone(), due.clone());
        tokio::spawn(scheduler.run());
        Self {
            frame_schedule_tx: tx,
            mode,
            due,
            paint_admitted: Arc::new(AtomicBool::new(true)),
            background_preparation: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Schedule a frame draw as soon as possible.
    pub fn schedule_frame(&self) {
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
        if self.mode() != PresentationMode::Frozen {
            let _ = self.frame_schedule_tx.send(FrameRequest {
                at: scheduling_now(),
                kind: FrameKind::Content,
                interactive: true,
            });
        }
    }

    /// Schedule an animation frame on the first beat after the specified duration.
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
        PresentationMode::decode(self.mode.load(Ordering::Relaxed))
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
}

#[cfg(test)]
impl FrameRequester {
    /// Create a no-op frame requester for tests.
    pub(crate) fn test_dummy() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        FrameRequester {
            frame_schedule_tx: tx,
            mode: Arc::new(AtomicU8::new(PresentationMode::Normal as u8)),
            due: Arc::new(AtomicU8::new(0)),
            paint_admitted: Arc::new(AtomicBool::new(true)),
            background_preparation: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// A scheduler for coalescing frame draw requests and notifying the TUI event loop.
///
/// This type is internal to `FrameRequester` and is spawned as a task to handle scheduling logic.
///
/// Completed-frame pacing is separate from coalescing notification demand.
struct FrameScheduler {
    receiver: mpsc::UnboundedReceiver<FrameRequest>,
    draw_tx: broadcast::Sender<()>,
    last_content_at: Option<Instant>,
    mode: Arc<AtomicU8>,
    due: Arc<AtomicU8>,
    /// Where the beat's grid starts.
    origin: Instant,
}

impl FrameScheduler {
    /// Create a new FrameScheduler with the provided receiver and draw notification sender.
    fn new(
        receiver: mpsc::UnboundedReceiver<FrameRequest>,
        draw_tx: broadcast::Sender<()>,
        mode: Arc<AtomicU8>,
        due: Arc<AtomicU8>,
    ) -> Self {
        Self {
            receiver,
            draw_tx,
            last_content_at: None,
            mode,
            due,
            origin: scheduling_now(),
        }
    }

    /// Run the scheduling loop, coalescing frame requests and notifying the TUI event loop.
    ///
    /// This method runs indefinitely until all senders are dropped. A single draw notification
    /// is sent for multiple requests scheduled before the next draw deadline.
    async fn run(self) {
        self.run_prepared().await
    }

    fn absorb_request(&mut self, deadlines: &mut [Option<Instant>; 4], request: FrameRequest) {
        self.absorb_prepared(deadlines, request)
    }
}
#[cfg(test)]
mod tests {
    use super::super::frame_rate_limiter::MIN_FRAME_INTERVAL;
    use super::beat::BEAT;
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
                    due: Arc::new(AtomicU8::new(0)),
                    paint_admitted: Arc::new(AtomicBool::new(true)),
                    background_preparation: Arc::new(AtomicBool::new(false)),
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
                    .timeout(UNFOCUSED_CONTENT_INTERVAL + Duration::from_millis(/*millis*/ 20))
                    .await,
                Ok(Ok(()))
            ),
            "coalesced content eventually paints"
        );
        assert!(
            draw_rx.try_recv().is_err(),
            "burst produced only one content paint"
        );
        requester.schedule_content_in(Duration::from_millis(/*millis*/ 200));
        requester.pay_all(); // A paint before the delayed content becomes visible.
        assert!(matches!(
            draw_rx
                .recv()
                .timeout(Duration::from_millis(/*millis*/ 250))
                .await,
            Ok(Ok(()))
        ));
        assert!(
            requester.pay_all().contains(Due::CONTENT),
            "due content must survive the preceding paint"
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
        requester.pay_all();

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
        assert!(requester.pay_all().contains(Due::CONTENT));

        time::advance(UNFOCUSED_CONTENT_INTERVAL).await;
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

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn animation_shares_the_beat_and_a_peripheral_window_keeps_only_the_glint() {
        /// Advances the clock, then lets the scheduler it woke run before the test looks.
        async fn advance(by: Duration) {
            time::advance(by).await;
            tokio::task::yield_now().await;
        }
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);
        requester.schedule_frame_in(Duration::ZERO);
        requester.schedule_frame_in(Duration::from_millis(/*millis*/ 5));
        requester.schedule_glint_in(Duration::ZERO);
        advance(BEAT - Duration::from_millis(/*millis*/ 1)).await;
        assert!(draw_rx.try_recv().is_err(), "motion waits for the beat");
        advance(Duration::from_millis(/*millis*/ 1)).await;
        assert!(
            draw_rx.try_recv().is_ok(),
            "concurrent motion shares one frame"
        );
        assert!(draw_rx.try_recv().is_err());
        assert_eq!(requester.pay_all(), Due::COSMETIC.with(Due::GLINT));

        advance(Duration::from_millis(/*millis*/ 1)).await;
        requester.set_mode(PresentationMode::Peripheral);
        advance(Duration::from_millis(/*millis*/ 1)).await;
        assert!(draw_rx.try_recv().is_ok(), "the transition paints once");
        assert_eq!(requester.pay_all(), Due::CONTENT);
        requester.schedule_frame_in(Duration::ZERO);
        requester.schedule_frame();
        requester.schedule_glint_in(Duration::ZERO);
        advance(BEAT * 2 - Duration::from_millis(/*millis*/ 3)).await;
        assert!(
            draw_rx.try_recv().is_err(),
            "a peripheral glint keeps the slower beat"
        );
        advance(Duration::from_millis(/*millis*/ 2)).await;
        assert!(draw_rx.try_recv().is_ok());
        assert_eq!(
            requester.pay_all(),
            Due::GLINT,
            "only the status row moves before content's coalescing deadline"
        );
        // The content arrived with the transition paint at 34ms and waits out its interval.
        advance(Duration::from_millis(/*millis*/ 56)).await;
        assert!(draw_rx.try_recv().is_err());
        advance(Duration::from_millis(/*millis*/ 2)).await;
        assert!(draw_rx.try_recv().is_ok());
        assert_eq!(requester.pay_all(), Due::CONTENT);
    }
}
