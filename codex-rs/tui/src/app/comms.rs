use super::App;
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol::CommsGate;
use codex_protocol::ThreadId;

impl App {
    pub(super) async fn comms(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        comms: Option<CommsGate>,
    ) {
        let result = match comms {
            Some(comms) => app_server.thread_comms_update(thread_id, comms).await,
            None => {
                app_server
                    .thread_read(thread_id, /*include_turns*/ false)
                    .await
            }
        };
        match result {
            Ok(thread) => self.chat_widget.show_comms(thread.comms),
            Err(err) => self.chat_widget.add_error_message(format!("Comms: {err}")),
        }
    }
}
