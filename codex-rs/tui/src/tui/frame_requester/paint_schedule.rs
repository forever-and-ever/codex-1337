//! Content cadence is independent of completed-frame paint admission; animation keeps the beat.

use super::Due;
use super::Duration;
use super::FrameKind;
use super::FrameRequest;
use super::FrameScheduler;
use super::Instant;
use super::PresentationMode;
use super::UNFOCUSED_CONTENT_INTERVAL;
use super::beat::BEAT;
use super::beat::next_beat;
use super::scheduling_now;

impl FrameScheduler {
    pub(super) async fn run_prepared(mut self) {
        const ONE_YEAR: Duration = Duration::from_hours(8760);
        let mut deadlines: [Option<Instant>; 4] = [None; 4];
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
                    let mode = self.mode();
                    let mut owed = Due::NONE;
                    if due[FrameKind::Content as usize] && mode != PresentationMode::Frozen {
                        // A preceding paint may have paid content before delayed content (for
                        // example a turn tip) became visible.
                        owed = owed.with(Due::CONTENT);
                        self.last_content_at = Some(now);
                    }
                    if due[FrameKind::Cosmetic as usize] && mode == PresentationMode::Normal {
                        owed = owed.with(Due::COSMETIC);
                    }
                    if due[FrameKind::Glint as usize] && mode.glints() {
                        owed = owed.with(Due::GLINT);
                    }
                    if owed != Due::NONE || due[FrameKind::Maintenance as usize] {
                        self.emit(owed);
                    }
                }
            }
        }
    }

    pub(super) fn absorb_prepared(
        &mut self,
        deadlines: &mut [Option<Instant>; 4],
        request: FrameRequest,
    ) {
        let mode = self.mode();
        let draw_at = match request.kind {
            FrameKind::ModeChanged => {
                // Every transition starts a new presentation epoch. In particular,
                // Frozen may have been entered and left before this actor is polled.
                for kind in [FrameKind::Content, FrameKind::Cosmetic, FrameKind::Glint] {
                    deadlines[kind as usize] = None;
                }
                self.last_content_at = None;
                return;
            }
            FrameKind::Content if mode.throttles_content() && !request.interactive => {
                self.last_content_at.map_or(request.at, |last| {
                    request.at.max(last + UNFOCUSED_CONTENT_INTERVAL)
                })
            }
            FrameKind::Content | FrameKind::Maintenance => request.at,
            FrameKind::Cosmetic if mode != PresentationMode::Normal => return,
            // Motion answering input paints at once; animations share the beat.
            FrameKind::Cosmetic if request.interactive => request.at,
            FrameKind::Cosmetic => next_beat(self.origin, BEAT, request.at),
            FrameKind::Glint => match mode.glint_beat() {
                Some(beat) => next_beat(self.origin, beat, request.at),
                None => return,
            },
        };
        match request.kind {
            // A content frame paints every animation due by then.
            FrameKind::Content
                if deadlines[FrameKind::Cosmetic as usize]
                    .is_some_and(|cosmetic| request.at <= cosmetic) =>
            {
                deadlines[FrameKind::Cosmetic as usize] = None;
            }
            FrameKind::Cosmetic
                if deadlines[FrameKind::Content as usize]
                    .is_some_and(|content| content <= draw_at) =>
            {
                return;
            }
            _ => {}
        }
        let deadline = &mut deadlines[request.kind as usize];
        *deadline = Some(deadline.map_or(draw_at, |cur| cur.min(draw_at)));
    }
}
