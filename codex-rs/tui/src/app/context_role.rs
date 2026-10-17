use super::App;
use super::turn_submissions::ui_requests::{UiRequestKind, UiRequestScope};
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol as api;
use codex_protocol::ThreadId;

use crate::chatwidget::ContextRoleCommand;
impl App {
    pub(super) async fn context_role(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        command: ContextRoleCommand,
    ) {
        let kind = match command {
            ContextRoleCommand::Show => UiRequestKind::ReadOnly,
            _ => UiRequestKind::Mutation,
        };
        let clear = matches!(command, ContextRoleCommand::Set(None));
        self.request_server(
            server,
            UiRequestScope::DisplayedThread(thread_id),
            kind,
            "Context role",
            move |handle| async move {
                let request_id = api::RequestId::String(uuid::Uuid::new_v4().to_string());
                match command {
                    ContextRoleCommand::Show => handle
                        .request_typed::<api::ThreadReadResponse>(api::ClientRequest::ThreadRead {
                            request_id,
                            params: api::ThreadReadParams {
                                thread_id: thread_id.to_string(),
                                include_turns: false,
                            },
                        })
                        .await
                        .map(|response| response.thread.context_role),
                    ContextRoleCommand::Set(role) => handle
                        .request_typed::<api::ThreadSetContextRoleResponse>(
                            api::ClientRequest::ThreadSetContextRole {
                                request_id,
                                params: api::ThreadSetContextRoleParams {
                                    thread_id: thread_id.to_string(),
                                    context_role: role.map(|role| role.as_str().to_owned()),
                                },
                            },
                        )
                        .await
                        .map(|response| response.context_role),
                }
            },
            move |app, _, result| {
                match result {
                    Ok(_) if clear => app.chat_widget.context_role_cleared(),
                    Ok(role) => app.chat_widget.show_context_role(role.as_deref()),
                    Err(error) => app
                        .chat_widget
                        .add_error_message(format!("Context role: {error}")),
                }
                Ok(())
            },
        );
    }
}
