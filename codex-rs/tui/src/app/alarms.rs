use super::App;
use crate::app_server_session::AppServerSession;
use codex_protocol::ThreadId;

impl App {
    pub(super) async fn disarm_alarm(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        id: String,
    ) {
        let current = self.chat_widget.thread_id() == Some(thread_id);
        if current && !self.chat_widget.begin_alarm_disarm(thread_id, &id) {
            return;
        }
        self.enqueue_server_operation(
            app_server,
            thread_id,
            super::turn_submissions::ServerOperation::Mutation(
                super::turn_submissions::ThreadMutation::DisarmAlarm(id),
            ),
        );
    }
}
