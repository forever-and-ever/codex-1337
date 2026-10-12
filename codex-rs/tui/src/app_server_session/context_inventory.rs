use super::AppServerSession;
use codex_app_server_protocol::{
    ClientRequest, ThreadContextInventoryParams, ThreadContextInventoryResponse,
};
use codex_protocol::{ContextInventory, ThreadId};
use color_eyre::eyre::{Result, WrapErr};
impl AppServerSession {
    pub(crate) async fn thread_context_inventory(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<ContextInventory> {
        let request_id = self.next_request_id();
        let response: ThreadContextInventoryResponse = self
            .client
            .request_typed(ClientRequest::ThreadContextInventory {
                request_id,
                params: ThreadContextInventoryParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/context/inventory failed")?;
        Ok(response.inventory)
    }
}
