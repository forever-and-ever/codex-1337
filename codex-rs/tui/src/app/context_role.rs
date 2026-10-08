use super::App;
use crate::app_server_session::AppServerSession;
use crate::chatwidget::ContextRoleCommand;
use codex_protocol::ThreadId;

impl App {
    pub(super) async fn context_role(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        command: ContextRoleCommand,
    ) {
        let result = match &command {
            ContextRoleCommand::Show => app_server
                .thread_read(thread_id, /*include_turns*/ false)
                .await
                .map(|thread| thread.context_role),
            ContextRoleCommand::Set(role) => {
                app_server
                    .thread_set_context_role(thread_id, role.as_ref())
                    .await
            }
        };
        match (command, result) {
            (ContextRoleCommand::Set(None), Ok(_)) => self.chat_widget.context_role_cleared(),
            (_, Ok(role)) => self.chat_widget.show_context_role(role.as_deref()),
            (_, Err(err)) => self
                .chat_widget
                .add_error_message(format!("Context role: {err}")),
        }
    }
}
