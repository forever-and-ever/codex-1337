//! A live UI enables preparation once; every widget shares its requester.

use super::FrameRateLimiter;
use super::FrameRequester;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

impl FrameRequester {
    pub(crate) fn enable_background_preparation(&self) {
        self.background_preparation.store(true, Ordering::Relaxed);
    }

    pub(crate) fn prepares_in_background(&self) -> bool {
        self.background_preparation.load(Ordering::Relaxed)
    }
}

impl FrameRequester {
    pub(crate) fn next_frame_at(
        started_at: Instant,
        ended_at: Instant,
        interactive: bool,
    ) -> Instant {
        let mut limiter = FrameRateLimiter::default();
        limiter.mark_completed(started_at, ended_at);
        if interactive {
            limiter.clamp_deadline_with_interval(ended_at, Duration::ZERO)
        } else {
            limiter.clamp_deadline(ended_at)
        }
    }

    pub(crate) fn set_paint_admitted(&self, admitted: bool) {
        self.paint_admitted.store(admitted, Ordering::Relaxed);
    }

    pub(crate) fn paint_admitted(&self) -> bool {
        self.paint_admitted.load(Ordering::Relaxed)
    }

    pub(crate) fn retain_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }
}
