use super::ThreadRequestProcessor;
use crate::error_code::{internal_error, invalid_params};
use codex_app_server_protocol::{ClientResponsePayload, JSONRPCErrorError, ThreadJiraParams};
use codex_protocol::ThreadId;

impl ThreadRequestProcessor {
    pub(crate) async fn thread_jira(&self, params: ThreadJiraParams) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let id = ThreadId::from_string(&params.thread_id).map_err(|error| invalid_params(error.to_string()))?;
        let thread = self.read_thread_view(id, /*include_turns*/ false).await.map_err(super::thread_read_view_error)?;
        let result = tokio::task::spawn_blocking(move || crate::jira_store::execute(&thread.cwd, params.expected_scope, params.action))
            .await.map_err(|error| internal_error(error.to_string()))?
            .map_err(invalid_params)?;
        Ok(Some(result.into()))
    }
}
