//! Checks incoming event ownership before consulting native-thread caches or UI state.
//! Connection-wide lifecycle and account events may only update the displayed conversation's UI.

use super::App;
use super::app_server_event_targets::ServerNotificationThreadTarget;
use super::app_server_event_targets::server_notification_thread_target;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::AppServerSessionEvent;
use crate::backend_id::BackendId;
use codex_app_server_client::AppServerEvent;

impl App {
    pub(super) async fn handle_polled_backend_event(
        &mut self,
        app_server: &AppServerSession,
        displayed_owner: BackendId,
        event: AppServerSessionEvent,
    ) {
        if event.backend_id != app_server.backend_id() {
            tracing::warn!("ignoring server event from a different backend");
            return;
        }
        if displayed_owner != event.backend_id {
            let AppServerEvent::ServerNotification(notification) = event.event else {
                return;
            };
            let ServerNotificationThreadTarget::Thread(thread_id) =
                server_notification_thread_target(&notification)
            else {
                return;
            };
            if self.voice_owner_thread_id() == Some(thread_id) {
                self.deliver_background_voice_notification(thread_id, &notification);
            }
            // TELOS(server-hosted-agents): an unfocused backend keeps its owned event custody.
            if self
                .thread_event_channels
                .get(&thread_id)
                .is_some_and(|channel| channel.backend_id == Some(event.backend_id))
                && let Err(error) = self
                    .enqueue_thread_notification(thread_id, *notification)
                    .await
            {
                tracing::warn!(%error, "failed to retain background backend notification");
            }
            return;
        }
        self.handle_backend_event(app_server, event).await;
    }

    pub(super) async fn handle_backend_event(
        &mut self,
        app_server: &AppServerSession,
        event: AppServerSessionEvent,
    ) {
        if event.backend_id != app_server.backend_id() {
            tracing::warn!("ignoring server event from a different backend");
            return;
        }
        self.handle_app_server_event(app_server, event.event).await;
    }
}
