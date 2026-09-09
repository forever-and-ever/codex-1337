//! Archive actions keep their row inert until the server acknowledges the mutation.

use super::*;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadArchiveResponse;
use codex_app_server_protocol::ThreadArchivedNotification;
use codex_app_server_protocol::ThreadStatus;

impl App {
    pub(super) fn archive_agents_overview_thread(
        &mut self,
        app_server: &AppServerSession,
        thread_id: ThreadId,
    ) {
        if self.agents_overview.archive_task.is_some() {
            return;
        }
        let allowed = self.primary_thread_id != Some(thread_id)
            && self
                .agents_overview
                .threads
                .get(&thread_id)
                .is_some_and(|thread| {
                    thread.as_ref().is_some_and(|thread| {
                        matches!(thread.status, ThreadStatus::Idle | ThreadStatus::NotLoaded)
                    })
                });
        if !allowed {
            self.finish_agents_overview_archive(
                thread_id,
                Err("Session is no longer available for archiving.".to_string()),
            );
            return;
        }
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        self.agents_overview.archive_task = Some(
            tokio::spawn(async move {
                let result = handle
                    .request_typed::<ThreadArchiveResponse>(ClientRequest::ThreadArchive {
                        request_id: RequestId::String(Uuid::new_v4().to_string()),
                        params: ThreadArchiveParams {
                            thread_id: thread_id.to_string(),
                        },
                    })
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                tx.send(AppEvent::AgentsOverviewThreadArchived { thread_id, result });
            })
            .abort_handle(),
        );
        self.repaint_agents_overview();
    }

    pub(super) fn finish_agents_overview_archive(
        &mut self,
        thread_id: ThreadId,
        result: Result<(), String>,
    ) {
        self.agents_overview.archive_task = None;
        if let Ok(mut state) = self.agents_overview.view_state.lock() {
            state.archiving = None;
            state.archive_error = result.as_ref().err().cloned();
        }
        if result.is_ok() {
            // Use the normal notification path so an older in-flight read cannot
            // resurrect the archived row after the acknowledgement.
            self.track_agents_overview_notification(&ServerNotification::ThreadArchived(
                ThreadArchivedNotification {
                    thread_id: thread_id.to_string(),
                },
            ));
        }
        self.repaint_agents_overview();
    }
}
