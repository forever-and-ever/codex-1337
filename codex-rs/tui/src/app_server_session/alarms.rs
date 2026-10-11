use super::AppServerSession;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ThreadAlarmDisarmParams;
use codex_app_server_protocol::ThreadAlarmDisarmResponse;
use codex_protocol::ThreadId;

impl AppServerSession {
    pub(crate) async fn thread_alarm_disarm(
        &mut self,
        thread_id: ThreadId,
        id: String,
    ) -> Result<ThreadAlarmDisarmResponse, String> {
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::ThreadAlarmDisarm {
                request_id,
                params: ThreadAlarmDisarmParams {
                    thread_id: thread_id.to_string(),
                    id,
                },
            })
            .await
            .map_err(|error| match error {
                TypedRequestError::Server { source, .. } => source.message,
                error => format!("Could not disarm alarm: {error}"),
            })
    }
}
