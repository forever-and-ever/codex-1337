use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

/// Transport metadata available while cleaning up an abandoned HTTP policy request.
/// This does not cancel the approval reviewer or change its decision.
#[derive(Clone, Debug, Default)]
pub struct NetworkRequestDisconnect(Arc<OnceLock<Duration>>);

impl NetworkRequestDisconnect {
    pub fn elapsed(&self) -> Option<Duration> {
        self.0.get().copied()
    }
}
