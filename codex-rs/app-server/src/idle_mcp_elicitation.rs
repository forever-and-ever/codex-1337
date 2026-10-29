use super::ClientRequestResult;
use super::RequestId;
use super::ServerRequestPayload;
use super::ThreadScopedOutgoingMessageSender;
use std::sync::Arc;
use tokio::sync::oneshot;

pub(super) fn is_idle_request(payload: &ServerRequestPayload) -> bool {
    matches!(
        payload,
        ServerRequestPayload::McpServerElicitationRequest(params)
            if params.turn_id.is_none()
                && !matches!(&params.request, codex_app_server_protocol::McpServerElicitationRequest::UserVerification { .. })
    )
}

impl ThreadScopedOutgoingMessageSender {
    pub(crate) fn with_idle_elicitation_connections(
        mut self,
        connections: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Self {
        self.idle_elicitation_connections = connections;
        self
    }

    pub(super) async fn send_idle_mcp_elicitation(
        &self,
        payload: ServerRequestPayload,
    ) -> (RequestId, oneshot::Receiver<ClientRequestResult>) {
        // TELOS(idle-mcp-human-consent): an absent frontend never grants consent.
        if self.connection_ids.is_empty()
            || self
                .idle_elicitation_connections
                .as_ref()
                .is_some_and(|watch| !*watch.borrow())
        {
            let (sender, receiver) = oneshot::channel();
            drop(sender);
            return (self.outgoing.next_request_id(), receiver);
        }
        let (id, pending) = self
            .outgoing
            .send_request_to_connections(
                Some(self.connection_ids.as_slice()),
                payload,
                Some(self.thread_id),
            )
            .await;
        let Some(mut connections) = self.idle_elicitation_connections.clone() else {
            return (id, pending);
        };
        let (mut sender, receiver) = oneshot::channel();
        let outgoing = Arc::clone(&self.outgoing);
        let request_id = id.clone();
        tokio::spawn(async move {
            tokio::select! {
                response = pending => {
                    if let Ok(response) = response {
                        let _ = sender.send(response);
                    }
                }
                () = async { let _ = connections.wait_for(|connected| !connected).await; } => {
                    outgoing.cancel_request(&request_id).await;
                    let _ = sender.send(Ok(serde_json::json!({"action": "decline", "content": null, "_meta": null})));
                }
                () = sender.closed() => {
                    outgoing.cancel_request(&request_id).await;
                }
            }
        });
        (id, receiver)
    }
}
