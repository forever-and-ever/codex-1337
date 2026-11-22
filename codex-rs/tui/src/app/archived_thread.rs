//! Project an archive notification onto the cached read-only conversation.

use super::App;
use super::thread_events::ThreadEventAttachment;
use crate::app_event::AppEvent;
use crate::chatwidget::ReadOnlyThread;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ServerNotification;
use codex_protocol::ThreadId;

pub(super) fn resume_reports_archived(error: &color_eyre::Report, thread_id: ThreadId) -> bool {
    matches!(
        error.downcast_ref::<TypedRequestError>(),
        Some(TypedRequestError::Server { method, source })
            if method == "thread/resume"
                && source.code == -32600
                && source.message.starts_with(&format!("session {thread_id} is archived. "))
    )
}

pub(super) fn update_archived_thread(
    app: &mut App,
    thread_id: ThreadId,
    notification: &ServerNotification,
) {
    let displayed = app.current_displayed_thread_id() == Some(thread_id);
    let Some(channel) = app.thread_event_channels.get_mut(&thread_id) else {
        return;
    };
    match notification {
        ServerNotification::ThreadArchived(_) => channel.mark_archived(),
        ServerNotification::ThreadUnarchived(_) if channel.is_archived() => {
            channel.mark_replay_only();
            if displayed {
                app.app_event_tx
                    .send(AppEvent::ResumeUnarchivedThread(thread_id));
            }
            return;
        }
        _ => return,
    }
    if displayed {
        show_cached_read_only_thread(app, thread_id);
    }
}

pub(super) fn show_cached_read_only_thread(app: &mut App, thread_id: ThreadId) {
    let Some(channel) = app.thread_event_channels.get(&thread_id) else {
        return;
    };
    let reason = match channel.attachment() {
        ThreadEventAttachment::Archived => ReadOnlyThread::Archived,
        ThreadEventAttachment::ExternalWriter => ReadOnlyThread::ExternalWriter,
        _ => return,
    };
    app.chat_widget.show_read_only_thread(reason);
}

pub(super) fn request_unarchive(
    app: &App,
    app_server: &crate::app_server_session::AppServerSession,
    thread_id: ThreadId,
) {
    let backend_id = app_server.backend_id();
    let mut session = app_server.request_view();
    let tx = app.app_event_tx.clone();
    tokio::spawn(async move {
        let result = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            session.thread_unarchive(thread_id),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(format!("Failed to unarchive thread: {error:#}")),
            Err(_) => Err("Unarchive wasn't confirmed. Check the task before retrying.".into()),
        };
        tx.send(AppEvent::UnarchiveFinished {
            backend_id,
            thread_id,
            result,
        });
    });
}
