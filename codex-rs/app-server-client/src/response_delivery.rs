//! Response delivery shares the client's existing command channel and one acknowledgement.

use super::*;

impl InProcessAppServerRequestHandle {
    /// Sends one response; an error after admission does not prove non-delivery.
    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> IoResult<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(ClientCommand::ResolveServerRequest {
                request_id,
                result,
                response_tx,
            })
            .await
            .map_err(|_| {
                IoError::new(
                    ErrorKind::BrokenPipe,
                    "in-process app-server worker channel is closed",
                )
            })?;
        response_rx.await.map_err(|_| {
            IoError::new(
                ErrorKind::BrokenPipe,
                "in-process app-server resolve channel is closed",
            )
        })?
    }
}

impl AppServerRequestHandle {
    /// Uses the same response capability as the owning client; never retries on lost acknowledgement.
    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> IoResult<()> {
        match self {
            Self::InProcess(handle) => handle.resolve_server_request(request_id, result).await,
            Self::Remote(handle) => handle.resolve_server_request(request_id, result).await,
        }
    }
}
