use super::ThreadRequestProcessor;
use crate::error_code::internal_error;
use crate::error_code::invalid_params;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadJiraParams;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;

impl ThreadRequestProcessor {
    pub(crate) async fn thread_jira(
        &self,
        params: ThreadJiraParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_params(error.to_string()))?;
        let thread = self
            .read_thread_view(id, /*include_turns*/ false)
            .await
            .map_err(super::thread_read_view_error)?;
        if !thread.cwd.as_path().is_absolute() {
            return Err(invalid_params("thread/jira requires an absolute local cwd"));
        }
        let cwd = AbsolutePathBuf::try_from(thread.cwd)
            .map_err(|error| invalid_params(error.to_string()))?;
        let result = tokio::task::spawn_blocking(move || {
            crate::jira_store::execute(
                cwd.as_path(),
                params.expected_scope.as_deref(),
                params.action,
            )
        })
        .await
        .map_err(|error| internal_error(error.to_string()))?
        .map_err(invalid_params)?;
        Ok(Some(result.into()))
    }
}
