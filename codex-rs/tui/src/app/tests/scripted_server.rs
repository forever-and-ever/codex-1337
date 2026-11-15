//! Scripted native transport for request-boundary witnesses, without a Cloud adapter.

use super::*;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use futures::SinkExt;
use futures::StreamExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

pub(in crate::app) struct ScriptedRequest {
    pub(in crate::app) request: Box<ClientRequest>,
    pub(in crate::app) response:
        oneshot::Sender<std::io::Result<std::result::Result<serde_json::Value, JSONRPCErrorError>>>,
}

pub(in crate::app) async fn scripted_server(
    mode: crate::app_server_session::ThreadParamsMode,
) -> Result<(
    AppServerSession,
    mpsc::UnboundedReceiver<ScriptedRequest>,
    JoinHandle<Result<()>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", listener.local_addr()?);
    let (requests, commands) = unbounded_channel();
    let service = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        while let Some(frame) = socket.next().await {
            let Message::Text(text) = frame? else {
                continue;
            };
            let JSONRPCMessage::Request(request) = serde_json::from_str(&text)? else {
                continue;
            };
            let id = request.id.clone();
            let reply = if request.method == "initialize" {
                JSONRPCMessage::Response(JSONRPCResponse {
                    id,
                    result: serde_json::json!({
                        "userAgent": "codex-tui-scripted-test",
                        "codexHome": "/unused",
                    }),
                })
            } else {
                let (response, result) = oneshot::channel();
                requests.send(ScriptedRequest {
                    request: Box::new(serde_json::from_value(serde_json::to_value(request)?)?),
                    response,
                })?;
                let Ok(result) = result.await else {
                    // A dropped reply models an uncertain native transport receipt.
                    return Ok(());
                };
                match result? {
                    Ok(result) => JSONRPCMessage::Response(JSONRPCResponse { id, result }),
                    Err(error) => JSONRPCMessage::Error(JSONRPCError { id, error }),
                }
            };
            socket
                .send(Message::Text(serde_json::to_string(&reply)?.into()))
                .await?;
        }
        Ok(())
    });
    let client = crate::connect_remote_app_server(crate::RemoteAppServerEndpoint::WebSocket {
        websocket_url,
        auth_token: None,
    })
    .await?;
    Ok((
        AppServerSession::new(crate::backend_id::BackendId::new(), client, mode),
        commands,
        service,
    ))
}
