//! Announce resolved core-created runtimes before their event reader starts.

use super::*;

struct ThreadAnnounced;

pub(super) async fn announce_created_thread(
    context: &ListenerTaskContext,
    thread_id: ThreadId,
    thread: &Arc<CodexThread>,
    snapshot: &codex_core::ThreadConfigSnapshot,
) {
    if !thread
        .thread_extension_data()
        .insert_if(ThreadAnnounced, |existing| existing.is_none())
    {
        return;
    }
    context
        .thread_watch_manager
        .upsert_thread(&thread_id.to_string())
        .await;
    let mut summary = super::super::thread_processor::build_thread_from_loaded_snapshot(
        thread_id, snapshot, thread,
    );
    summary.status = context
        .thread_watch_manager
        .loaded_status_for_thread(&summary.id)
        .await;
    context
        .outgoing
        .send_server_notification(ServerNotification::ThreadStarted(
            super::super::thread_summary::thread_started_notification(summary),
        ))
        .await;
}
