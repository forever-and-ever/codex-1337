//! How long the first `exec_command` call waits before it hands a still-running process back.

use tokio::time::Duration;
use tokio::time::Instant;

use super::clamp_yield_time;

/// How long the first `exec_command` call waits before it hands a still-running process back
/// as a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InitialWait {
    /// The requested milliseconds, clamped to the unified-exec yield range.
    Yield(u64),
    /// Until the process exits. A Code Mode cell waits this way: the cell, not the command,
    /// returns control to the model when news arrives or the cell's own budget runs out.
    Exit,
}

impl InitialWait {
    /// When output collection stops waiting for the process.
    pub(crate) fn deadline(self, start: Instant) -> Instant {
        match self {
            Self::Yield(yield_time_ms) => {
                start + Duration::from_millis(clamp_yield_time(yield_time_ms))
            }
            Self::Exit => start + EXIT_HORIZON,
        }
    }
}

/// Outlives every cell and turn; collection still ends the moment the process exits.
const EXIT_HORIZON: Duration = Duration::from_secs(365 * 24 * 60 * 60);
