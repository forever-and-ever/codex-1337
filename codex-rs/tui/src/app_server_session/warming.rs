use super::AppServerSession;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ThreadSetWarmingParams;
use codex_app_server_protocol::ThreadSetWarmingResponse;
use codex_protocol::ThreadId;

impl AppServerSession {
    /// A refusal carries the server's reason verbatim.
    pub(crate) async fn thread_set_warming(
        &mut self,
        thread_id: ThreadId,
        enabled: bool,
    ) -> Result<ThreadSetWarmingResponse, String> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ThreadSetWarming {
                request_id,
                params: ThreadSetWarmingParams {
                    thread_id: thread_id.to_string(),
                    enabled,
                },
            })
            .await
            .map_err(|error| match error {
                TypedRequestError::Server { source, .. } => source.message,
                error => format!("Warming failed: {error}"),
            })
    }
}
