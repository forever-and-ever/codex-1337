use super::Session;
use std::sync::atomic::Ordering;

impl Session {
    fn next_fork_recording_id(&self) -> u64 {
        self.next_fork_recording_id.fetch_add(1, Ordering::SeqCst)
    }

    pub(super) fn next_internal_event_id(&self) -> String {
        let id = self.next_fork_recording_id();
        format!("internal-{id}")
    }

    pub(super) async fn inject_items_sub_id(&self) -> String {
        self.active_turn
            .lock()
            .await
            .as_ref()
            .and_then(|turn| turn.task.as_ref())
            .map_or_else(
                || {
                    let id = self.next_fork_recording_id();
                    format!("inject-{id}")
                },
                |task| task.turn_context.sub_id.clone(),
            )
    }
}
