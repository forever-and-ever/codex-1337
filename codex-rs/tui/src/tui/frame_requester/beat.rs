//! The beat: animations keep one clock, and the status row glints between full frames.
//!
//! Cosmetic deadlines land on a grid every animation shares, so concurrent motion shares its
//! frames instead of interleaving them. A glint is the status row's motion alone; a visible but
//! unfocused window keeps only that, on a slower beat.

use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use super::FrameKind;
use super::FrameRequest;
use super::FrameRequester;
use super::FrameScheduler;
use super::PresentationMode;
use super::scheduling_now;

/// The beat a focused window animates on: the shimmer's own tick.
pub(super) const BEAT: Duration = libshimmer::TICK;
/// The slower beat a peripheral window's status row glints on.
const PERIPHERAL_BEAT: Duration = BEAT.saturating_mul(3);

/// What the screen owes since its last full frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Due(u8);

impl Due {
    pub(crate) const NONE: Self = Self(0);
    /// Meaningful state awaits presentation.
    pub(crate) const CONTENT: Self = Self(1);
    /// Animation moved, perhaps beyond the status row.
    pub(crate) const COSMETIC: Self = Self(1 << 1);
    /// The status row moved.
    pub(crate) const GLINT: Self = Self(1 << 2);

    pub(crate) fn contains(self, debt: Self) -> bool {
        self.0 & debt.0 == debt.0
    }

    pub(super) fn with(self, debt: Self) -> Self {
        Self(self.0 | debt.0)
    }
}

impl PresentationMode {
    pub(super) fn decode(mode: u8) -> Self {
        match mode {
            0 => Self::Normal,
            1 => Self::Peripheral,
            2 => Self::Reduced,
            3 => Self::Frozen,
            _ => unreachable!("presentation mode must be set through set_mode"),
        }
    }

    /// The beat the status row glints on, where it moves at all.
    pub(super) fn glint_beat(self) -> Option<Duration> {
        match self {
            Self::Normal => Some(BEAT),
            Self::Peripheral => Some(PERIPHERAL_BEAT),
            Self::Reduced | Self::Frozen => None,
        }
    }

    /// Whether the status row moves.
    pub(crate) fn glints(self) -> bool {
        self.glint_beat().is_some()
    }

    /// Whether unattended content waits out a coalescing interval, which input must not.
    pub(crate) fn throttles_content(self) -> bool {
        matches!(self, Self::Peripheral | Self::Reduced)
    }
}

/// The first instant of `beat`'s grid from `origin` strictly after `at`.
pub(super) fn next_beat(origin: Instant, beat: Duration, at: Instant) -> Instant {
    let phase = at.saturating_duration_since(origin).as_nanos() % beat.as_nanos();
    // The phase is shorter than a beat, so its nanoseconds fit.
    at + beat.saturating_sub(Duration::from_nanos(
        u64::try_from(phase).unwrap_or_default(),
    ))
}

impl FrameRequester {
    /// Asks for the status row's next glint, on the first beat after `after`.
    pub(crate) fn schedule_glint_in(&self, after: Duration) {
        if self.mode().glints() {
            let _ = self.frame_schedule_tx.send(FrameRequest {
                at: scheduling_now() + after,
                kind: FrameKind::Glint,
                interactive: false,
            });
        }
    }

    /// Motion answering input, such as scrolling or a dragged selection, paints off the beat.
    pub(crate) fn schedule_interactive_frame_in(&self, after: Duration) {
        if self.mode() == PresentationMode::Normal {
            let _ = self.frame_schedule_tx.send(FrameRequest {
                at: scheduling_now() + after,
                kind: FrameKind::Cosmetic,
                interactive: true,
            });
        }
    }

    /// Wakes the event loop to pay what the screen already owes, adding no debt of its own.
    pub(crate) fn retry_paint(&self) {
        self.schedule_maintenance_in(Duration::ZERO);
    }

    /// What the screen owes, left owing.
    pub(crate) fn owed(&self) -> Due {
        Due(self.due.load(Ordering::Relaxed))
    }

    pub(crate) fn pay(&self, debt: Due) {
        self.due.fetch_and(!debt.0, Ordering::Relaxed);
    }

    /// Pays everything owed, as a full frame does.
    pub(crate) fn pay_all(&self) -> Due {
        Due(self.due.swap(0, Ordering::Relaxed))
    }
}

impl FrameScheduler {
    pub(super) fn mode(&self) -> PresentationMode {
        PresentationMode::decode(self.mode.load(Ordering::Relaxed))
    }

    /// Wakes the event loop, owing `debt`.
    pub(super) fn emit(&self, debt: Due) {
        self.due.fetch_or(debt.0, Ordering::Relaxed);
        let _ = self.draw_tx.send(());
    }
}
