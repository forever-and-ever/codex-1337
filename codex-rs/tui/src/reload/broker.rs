//! The server connection outlives UI generations. Disconnecting a UI is not a server detach.
//!
//! Requests admitted by an old generation run to completion; they are never replayed.
//! Pending server requests are retained until explicitly answered. The supervisor, not
//! this asynchronous transport, owns the reload deadline.

use codex_app_server_client::AppServerEvent;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_app_server_protocol::ClientNotification;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use futures::SinkExt;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;

const MAILBOX: usize = 256;

pub(super) enum Command {
    Connect(RemoteAppServerEndpoint, oneshot::Sender<io::Result<()>>),
    Attach(u64, mpsc::Sender<Message>),
    Input(u64, JSONRPCMessage),
    Detach(u64),
}

pub(super) async fn listen(
    listener: UnixListener,
    commands: mpsc::Sender<Command>,
) -> io::Result<()> {
    let mut generation = 0;
    loop {
        let (stream, _) = listener.accept().await?;
        generation += 1;
        let commands = commands.clone();
        tokio::spawn(async move {
            let Ok(socket) = tokio_tungstenite::accept_async(stream).await else {
                return;
            };
            let (mut sink, mut source) = socket.split();
            let (output, mut writes) = mpsc::channel(MAILBOX);
            if commands
                .send(Command::Attach(generation, output))
                .await
                .is_err()
            {
                return;
            }
            loop {
                tokio::select! {
                    message = writes.recv() => match message {
                        Some(message) => { if sink.send(message).await.is_err() { break } },
                        _ => break,
                    },
                    message = source.next() => match message {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(message) = serde_json::from_str(&text) else { break };
                            if commands.send(Command::Input(generation, message)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Ping(bytes))) => {
                            if sink.send(Message::Pong(bytes)).await.is_err() { break }
                        }
                        Some(Ok(Message::Pong(_))) => {},
                        _ => break,
                    }
                }
            }
            let _ = commands.send(Command::Detach(generation)).await;
        });
    }
}

pub(super) async fn run(mut commands: mpsc::Receiver<Command>) {
    let mut upstream: Option<RemoteAppServerClient> = None;
    let mut connected_endpoint = None;
    let mut ui: Option<(u64, mpsc::Sender<Message>)> = None;
    let mut ui_generation = 0;
    let mut pending = HashMap::<RequestId, Value>::new();
    let mut request_sequence: u64 = 0;
    let (results, mut replies) = mpsc::channel::<(u64, JSONRPCMessage)>(MAILBOX);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Connect(endpoint, response)) => {
                    if connected_endpoint.as_ref().is_some_and(|connected| connected != &endpoint) {
                        let _ = response.send(Err(io::Error::other("A reload cannot change this terminal's app server")));
                        continue;
                    }
                    if upstream.is_some() {
                        let _ = response.send(Ok(()));
                        continue;
                    }
                    match RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
                        endpoint: endpoint.clone(),
                        client_name: "codex-tui".into(),
                        client_version: env!("CARGO_PKG_VERSION").into(),
                        experimental_api: true,
                        mcp_server_openai_form_elicitation: false,
                        opt_out_notification_methods: Vec::new(),
                        channel_capacity: MAILBOX,
                    }).await {
                        Ok(client) => {
                            upstream = Some(client);
                            connected_endpoint = Some(endpoint);
                            let _ = response.send(Ok(()));
                        }
                        Err(error) => { let _ = response.send(Err(error)); }
                    }
                }
                Some(Command::Attach(generation, writer)) => {
                    // A handshake begun by a dead worker may finish after its replacement.
                    if generation > ui_generation {
                        ui_generation = generation;
                        ui = Some((generation, writer));
                    }
                }
                Some(Command::Detach(generation)) => {
                    if ui.as_ref().is_some_and(|(current, _)| *current == generation) {
                        ui = None;
                    }
                }
                Some(Command::Input(generation, message)) => {
                    if ui.as_ref().is_none_or(|(current, _)| *current != generation) {
                        continue;
                    }
                    let Some(client) = upstream.as_ref() else { continue };
                    match message {
                        JSONRPCMessage::Request(request) if request.method == "initialize" => {
                            send(&mut ui, &JSONRPCMessage::Response(JSONRPCResponse {
                                id: request.id,
                                result: serde_json::json!({
                                    "userAgent": "codex-ui-host",
                                    "serverVersion": client.server_version(),
                                    "codexHome": client.codex_home(),
                                    "platformFamily": client.platform_family(),
                                    "platformOs": client.platform_os(),
                                }),
                            }));
                        }
                        JSONRPCMessage::Request(mut request) => {
                            let id = request.id.clone();
                            request_sequence += 1;
                            request.id = RequestId::String(format!("ui-host-{request_sequence}"));
                            let handle = client.request_handle();
                            let results = results.clone();
                            tokio::spawn(async move {
                                let result = handle.request_json_rpc(request).await;
                                let message = match result {
                                    Ok(Ok(result)) => JSONRPCMessage::Response(JSONRPCResponse { id, result }),
                                    Ok(Err(error)) => JSONRPCMessage::Error(JSONRPCError { id, error }),
                                    Err(error) => JSONRPCMessage::Error(JSONRPCError {
                                        id,
                                        error: JSONRPCErrorError {
                                            code: -32000,
                                            message: error.to_string(),
                                            data: None,
                                        },
                                    }),
                                };
                                let _ = results.send((generation, message)).await;
                            });
                        }
                        JSONRPCMessage::Notification(notification) => {
                            if notification.method == "initialized" {
                                for request in pending.values() { send(&mut ui, request); }
                            } else if let Ok(notification) = serde_json::to_value(notification)
                                .and_then(serde_json::from_value::<ClientNotification>) {
                                let _ = client.notify(notification).await;
                            }
                        }
                        JSONRPCMessage::Response(response) => {
                            if pending.contains_key(&response.id)
                                && client.resolve_server_request(response.id.clone(), response.result).await.is_ok()
                            {
                                pending.remove(&response.id);
                            }
                        }
                        JSONRPCMessage::Error(error) => {
                            if pending.contains_key(&error.id)
                                && client.reject_server_request(error.id.clone(), error.error).await.is_ok()
                            {
                                pending.remove(&error.id);
                            }
                        }
                    }
                }
                None => break,
            },
            event = async {
                match &mut upstream {
                    Some(client) => client.next_event().await,
                    None => std::future::pending().await,
                }
            } => match event {
                Some(AppServerEvent::ServerNotification(notification)) => {
                    if let codex_app_server_protocol::ServerNotification::ServerRequestResolved(value) = notification.as_ref() {
                        pending.remove(&value.request_id);
                    }
                    send(&mut ui, &notification);
                }
                Some(AppServerEvent::ServerRequest(request)) => {
                    if let Ok(value) = serde_json::to_value(&request)
                        && let Some(id) = value.get("id").and_then(|id| serde_json::from_value(id.clone()).ok()) {
                            pending.insert(id, value.clone());
                            send(&mut ui, &value);
                        }
                }
                Some(AppServerEvent::Lagged { .. } | AppServerEvent::Disconnected { .. }) | None => {
                    ui = None;
                    upstream = None;
                    pending.clear();
                }
            },
            Some((generation, message)) = replies.recv() => {
                if ui.as_ref().is_some_and(|(current, _)| *current == generation) {
                    send(&mut ui, &message);
                }
            }

        }
    }
}

fn send(ui: &mut Option<(u64, mpsc::Sender<Message>)>, value: &impl serde::Serialize) {
    let Some((_, writer)) = ui else { return };
    let Ok(text) = serde_json::to_string(value) else {
        *ui = None;
        return;
    };
    // A slow/dead UI cannot stall the persistent transport. Reattachment hydrates history.
    if writer.try_send(Message::Text(text.into())).is_err() {
        *ui = None;
    }
}
