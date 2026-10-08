use super::ThreadRequestProcessor;
use crate::error_code::invalid_request;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadSetWarmingParams;
use codex_protocol::ThreadId;

impl ThreadRequestProcessor {
    pub(crate) async fn thread_set_warming(
        &self,
        params: ThreadSetWarmingParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request("Open the conversation before warming it."))?;
        let response = self
            .prompt_cache
            .set_warming(thread_id, thread.as_ref(), params.enabled)
            .await
            .map_err(invalid_request)?;
        Ok(Some(response.into()))
    }
}
