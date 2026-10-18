//! Retirement affects agent-family membership, not the saved conversation.
use super::JSONRPCErrorError;
use super::Result;
use super::ThreadId;
use super::ThreadRequestProcessor;
use super::invalid_request;
use codex_app_server_protocol::ThreadRetireParams;
use codex_app_server_protocol::ThreadRetireResponse;

impl ThreadRequestProcessor {
    pub(crate) async fn thread_retire(
        &self,
        params: ThreadRetireParams,
    ) -> Result<ThreadRetireResponse, JSONRPCErrorError> {
        let root = ThreadId::from_string(&params.root_thread_id)
            .map_err(|err| invalid_request(format!("invalid root thread id: {err}")))?;
        let target = ThreadId::from_string(&params.thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let _permit = self.acquire_thread_list_state_permit().await?;
        let ids = self
            .thread_manager
            .retire_agent(root, target)
            .await
            .map_err(|err| invalid_request(err.to_string()))?;
        Ok(ThreadRetireResponse {
            thread_ids: ids.into_iter().map(|id| id.to_string()).collect(),
        })
    }
}
