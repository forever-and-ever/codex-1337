//! Freeze a realtime split-flap cell for prepared stream replacement.

use super::*;

impl SplitFlapTranscriptCell {
    pub(in crate::chatwidget) fn snapshot(&self, width: u16) -> Self {
        use super::super::activity_presentation::LiveSnapshot;
        Self {
            inner: self.inner.as_ref().snapshot(width),
            role: self.role.clone(),
            target: self.target.clone(),
            visible_target: self.visible_target.clone(),
            flap_sample: self.flap_sample.clone(),
            tile_arrivals: self.tile_arrivals.clone(),
            started_at: self.started_at,
            phase_started_at: self.phase_started_at,
            motion_mode: self.motion_mode,
            frame_requester: self.frame_requester.clone(),
        }
    }
}
