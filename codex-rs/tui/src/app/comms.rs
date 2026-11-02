use super::App;
use super::turn_submissions::ui_requests::UiRequestKind;
use super::turn_submissions::ui_requests::UiRequestScope;
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol as api;
use codex_protocol::ThreadId;

use api::CommsGate;
impl App {
    pub(super) fn comms(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        comms: Option<CommsGate>,
    ) {
        let kind = if comms.is_some() {
            UiRequestKind::Mutation
        } else {
            UiRequestKind::ReadOnly
        };
        self.request_server(
            server,
            UiRequestScope::DisplayedThread(thread_id),
            kind,
            "Comms",
            move |handle| async move {
                let request_id = api::RequestId::String(uuid::Uuid::new_v4().to_string());
                match comms {
                    Some(comms) => handle
                        .request_typed::<api::ThreadMetadataUpdateResponse>(
                            api::ClientRequest::ThreadMetadataUpdate {
                                request_id,
                                params: api::ThreadMetadataUpdateParams {
                                    theme: None,
                                    thread_id: thread_id.to_string(),
                                    comms: Some(comms),
                                    project_id: None,
                                    daybreak_enabled: None,
                                    git_info: None,
                                },
                            },
                        )
                        .await
                        .map(|response| response.thread.comms),
                    None => handle
                        .request_typed::<api::ThreadReadResponse>(api::ClientRequest::ThreadRead {
                            request_id,
                            params: api::ThreadReadParams {
                                thread_id: thread_id.to_string(),
                                include_turns: false,
                            },
                        })
                        .await
                        .map(|response| response.thread.comms),
                }
            },
            |app, _, result| {
                match result {
                    Ok(comms) => app.chat_widget.show_comms(comms),
                    Err(error) => app.chat_widget.add_error_message(format!("Comms: {error}")),
                }
                Ok(())
            },
        );
    }
}
