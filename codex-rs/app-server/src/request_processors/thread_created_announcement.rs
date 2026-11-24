//! Announce resolved core-created runtimes before their event reader starts.

use super::ListenerTaskContext;
use super::thread_processor::build_thread_from_loaded_snapshot;
use super::thread_summary::thread_started_notification;
use codex_app_server_protocol::ServerNotification;
use codex_core::CodexThread;
use codex_core::ThreadConfigSnapshot;
use codex_protocol::ThreadId;
use std::sync::Arc;

struct ThreadAnnounced;

pub(super) async fn announce_created_thread(
    context: &ListenerTaskContext,
    thread_id: ThreadId,
    thread: &Arc<CodexThread>,
    snapshot: &ThreadConfigSnapshot,
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
    let mut summary = match build_thread_from_loaded_snapshot(thread, snapshot) {
        Ok(summary) => summary,
        Err(error) => {
            tracing::warn!(%thread_id, message = %error.message, "cannot announce thread");
            return;
        }
    };
    summary.status = context
        .thread_watch_manager
        .loaded_status_for_thread(&summary.id)
        .await;
    context
        .outgoing
        .send_server_notification(ServerNotification::ThreadStarted(
            thread_started_notification(summary),
        ))
        .await;
}
