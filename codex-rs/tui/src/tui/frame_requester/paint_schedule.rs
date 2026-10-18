//! Content cadence is independent of completed-frame paint admission.

use super::Duration;
use super::FrameKind;
use super::FrameRequest;
use super::FrameScheduler;
use super::Instant;
use super::Ordering;
use super::PresentationMode;
use super::REDUCED_CONTENT_INTERVAL;
use super::scheduling_now;

impl FrameScheduler {
    pub(super) async fn run_prepared(mut self) {
        const ONE_YEAR: Duration = Duration::from_hours(8760);
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
                }
                () = &mut deadline => {
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
                            // A preceding paint may have cleared dirty before delayed
                            // content (for example a turn tip) became visible.
                            if due[FrameKind::Content as usize] {
                                self.dirty.store(true, Ordering::Relaxed);
                            }
                            if due[FrameKind::Content as usize] {
                                self.last_content_at = Some(scheduling_now());
                            }
                            let _ = self.draw_tx.send(());
                    }
                }
            }
        }
    }

    pub(super) fn absorb_prepared(
        &mut self,
        deadlines: &mut [Option<Instant>; 3],
        request: FrameRequest,
    ) {
        if matches!(request.kind, FrameKind::ModeChanged) {
            // Every transition starts a new presentation epoch. In particular,
            // Frozen may have been entered and left before this actor is polled.
            deadlines[FrameKind::Cosmetic as usize] = None;
            deadlines[FrameKind::Content as usize] = None;
            self.last_content_at = None;
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
            self.last_content_at.map_or(request.at, |last| {
                request.at.max(last + REDUCED_CONTENT_INTERVAL)
            })
        } else {
            request.at
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
