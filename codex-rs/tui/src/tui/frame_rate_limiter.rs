//! Frame pacing follows completed UI work, not notification emission.

mod completion;
pub(super) use completion::FrameRateLimiter;
pub(super) use completion::MIN_FRAME_INTERVAL;
