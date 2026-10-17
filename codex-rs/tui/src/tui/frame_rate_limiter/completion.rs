//! Paces painting from completed UI frames, never from draw notifications.
//!
//! Passive frames rest for at least 8.33 ms or their preceding UI-thread cost, whichever is
//! longer. Interactive frames rest only for that cost; their input supply is self-limiting.

use std::time::Duration;
use std::time::Instant;

/// A 120 FPS minimum frame interval (≈8.33ms).
pub(in crate::tui) const MIN_FRAME_INTERVAL: Duration = Duration::from_nanos(8_333_334);

/// Keeps the completion and UI-thread cost of the preceding frame.
#[derive(Debug, Default)]
pub(in crate::tui) struct FrameRateLimiter {
    last_completed_at: Option<Instant>,
    last_frame_cost: Duration,
}

impl FrameRateLimiter {
    /// Returns `requested`, clamped forward if it would exceed the maximum frame rate.
    pub(in crate::tui) fn clamp_deadline(&self, requested: Instant) -> Instant {
        self.clamp_deadline_with_interval(requested, MIN_FRAME_INTERVAL)
    }

    pub(in crate::tui) fn clamp_deadline_with_interval(
        &self,
        requested: Instant,
        interval: Duration,
    ) -> Instant {
        let Some(last_completed_at) = self.last_completed_at else {
            return requested;
        };
        let min_allowed = last_completed_at
            .checked_add(interval.max(self.last_frame_cost))
            .unwrap_or(last_completed_at);
        requested.max(min_allowed)
    }

    pub(in crate::tui) fn mark_completed(&mut self, started_at: Instant, ended_at: Instant) {
        self.last_completed_at = Some(ended_at);
        self.last_frame_cost = ended_at.saturating_duration_since(started_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn default_does_not_clamp() {
        let t0 = Instant::now();
        let limiter = FrameRateLimiter::default();
        assert_eq!(limiter.clamp_deadline(t0), t0);
    }

    #[test]
    fn completion_cost_and_requested_deadlines_bound_painting() {
        let t0 = Instant::now();
        let mut limiter = FrameRateLimiter::default();

        assert_eq!(limiter.clamp_deadline(t0), t0);
        limiter.mark_completed(t0, t0);

        let too_soon = t0 + Duration::from_millis(1);
        assert_eq!(limiter.clamp_deadline(too_soon), t0 + MIN_FRAME_INTERVAL);

        let frame_end = t0 + Duration::from_millis(30);
        limiter.mark_completed(t0, frame_end);
        assert_eq!(
            limiter.clamp_deadline(frame_end),
            frame_end + Duration::from_millis(30)
        );
        let next_end = frame_end + Duration::from_millis(35);
        limiter.mark_completed(next_end - Duration::from_millis(1), next_end);
        assert_eq!(
            limiter.clamp_deadline(next_end),
            next_end + MIN_FRAME_INTERVAL
        );
        assert_eq!(
            limiter.clamp_deadline(frame_end - Duration::from_millis(20)),
            next_end + MIN_FRAME_INTERVAL
        );
        assert_eq!(
            limiter.clamp_deadline(next_end + Duration::from_millis(100)),
            next_end + Duration::from_millis(100)
        );
        assert_eq!(
            limiter.clamp_deadline_with_interval(next_end, Duration::ZERO),
            next_end + Duration::from_millis(1)
        );
    }
}
