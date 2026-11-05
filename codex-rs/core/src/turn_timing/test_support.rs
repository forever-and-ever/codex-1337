//! Event-driven integration-test admission to the tool-blocking measurement window.
use super::TurnProfilePhase;
use super::TurnTimingState;

impl TurnTimingState {
    pub(crate) async fn wait_for_tool_blocking(&self) -> bool {
        loop {
            let changed = self.tool_blocking_started.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.profile_state();
                if state.completed_profile.is_some() {
                    return false;
                }
                if state.active_phase == Some(TurnProfilePhase::ToolBlocking) {
                    return true;
                }
            }
            changed.await;
        }
    }
}
