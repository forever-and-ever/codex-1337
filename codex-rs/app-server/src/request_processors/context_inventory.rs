use super::ClientResponsePayload;
use super::JSONRPCErrorError;
use super::Result;
use super::ThreadId;
use super::ThreadRequestProcessor;
use super::invalid_request;
use codex_app_server_protocol::ThreadContextInventoryParams;
use codex_app_server_protocol::ThreadContextInventoryResponse;
impl ThreadRequestProcessor {
    pub(crate) async fn thread_context_inventory(
        &self,
        params: ThreadContextInventoryParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|error| invalid_request(format!("Thread is not loaded: {error}")))?;
        let inventory = thread.context_inventory().await.map_err(invalid_request)?;
        Ok(Some(ThreadContextInventoryResponse { inventory }.into()))
    }
}
