use super::App;
use super::turn_submissions::ui_requests::{UiRequestKind, UiRequestScope};
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol as api;
use codex_protocol::ThreadId;

use super::turn_submissions::SubmissionFailure;
use codex_app_server_client::TypedRequestError;
impl App {
    /// The server broadcasts the resulting state; only a refusal needs reporting here.
    pub(super) async fn set_warming(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        enabled: bool,
    ) {
        self.request_server(
            server,
            UiRequestScope::Thread(thread_id),
            UiRequestKind::Mutation,
            "Warming",
            move |handle| async move {
                handle
                    .request_typed::<api::ThreadSetWarmingResponse>(
                        api::ClientRequest::ThreadSetWarming {
                            request_id: api::RequestId::String(uuid::Uuid::new_v4().to_string()),
                            params: api::ThreadSetWarmingParams {
                                thread_id: thread_id.to_string(),
                                enabled,
                            },
                        },
                    )
                    .await
            },
            |app, _, result| {
                if let Err(error) = result {
                    let message = match error {
                        SubmissionFailure::Rejected(TypedRequestError::Server {
                            source, ..
                        }) => source.message.clone(),
                        error => format!("Warming failed: {error}"),
                    };
                    app.chat_widget.add_error_message(message);
                }
                Ok(())
            },
        );
    }
}
