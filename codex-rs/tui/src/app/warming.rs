use super::App;
use crate::app_server_session::AppServerSession;
use codex_protocol::ThreadId;

impl App {
    /// The server broadcasts the resulting state; only a refusal needs reporting here.
    pub(super) async fn set_warming(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        enabled: bool,
    ) {
        if let Err(message) = app_server.thread_set_warming(thread_id, enabled).await {
            self.chat_widget.add_error_message(message);
        }
    }
}
