//! The server connection outlives UI connections. Disconnecting a UI is not a server detach.
//!
//! Requests admitted by an old connection run to completion; they are never replayed.
//! Replies return to their originating connection; temporary config/history connections
//! coexist with the main UI. Notifications fan out through bounded, nonblocking queues:
//! a stalled connection is dropped alone and can reattach to hydrate history.
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
use tokio_util::sync::CancellationToken;
use tokio_util::sync::DropGuard;

const MAILBOX: usize = 256;

pub(super) struct Connection {
    writer: mpsc::Sender<Message>,
    _disconnect: DropGuard,
}

pub(super) enum Command {
    Connect(RemoteAppServerEndpoint, oneshot::Sender<io::Result<()>>),
    Attach(u64, Connection),
    Input(u64, JSONRPCMessage),
    Detach(u64),
}

pub(super) async fn listen(
    listener: UnixListener,
    commands: mpsc::Sender<Command>,
) -> io::Result<()> {
    let mut connection = 0;
    loop {
        let (stream, _) = listener.accept().await?;
        connection += 1;
        let commands = commands.clone();
        tokio::spawn(async move {
            let Ok(socket) = tokio_tungstenite::accept_async(stream).await else {
                return;
            };
            let (mut sink, mut source) = socket.split();
            let (output, mut writes) = mpsc::channel(MAILBOX);
            let disconnect = CancellationToken::new();
            let writer = Connection {
                writer: output,
                _disconnect: disconnect.clone().drop_guard(),
            };
            if commands
                .send(Command::Attach(connection, writer))
                .await
                .is_err()
            {
                return;
            }
            let forwarding = async {
                loop {
                    tokio::select! {
                        message = writes.recv() => match message {
                            Some(message) => { if sink.send(message).await.is_err() { break } },
                            _ => break,
                        },
                        message = source.next() => match message {
                            Some(Ok(Message::Text(text))) => {
                                let Ok(message) = serde_json::from_str(&text) else { break };
                                if commands.send(Command::Input(connection, message)).await.is_err() {
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
            };
            tokio::select! {
                () = disconnect.cancelled() => {},
                () = forwarding => {},
            }
            let _ = commands.send(Command::Detach(connection)).await;
        });
    }
}

pub(super) async fn run(mut commands: mpsc::Receiver<Command>) {
    let mut upstream: Option<RemoteAppServerClient> = None;
    let mut connected_endpoint = None;
    let mut ui = HashMap::<u64, Connection>::new();
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
                        required_human_approval: true,
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
                Some(Command::Attach(connection, writer)) => {
                    ui.insert(connection, writer);
                }
                Some(Command::Detach(connection)) => {
                    ui.remove(&connection);
                }
                Some(Command::Input(connection, message)) => {
                    if !ui.contains_key(&connection) {
                        continue;
                    }
                    let Some(client) = upstream.as_ref() else { continue };
                    match message {
                        JSONRPCMessage::Request(request) if request.method == "initialize" => {
                            send(&mut ui, &Audience::Connection(connection), &JSONRPCMessage::Response(JSONRPCResponse {
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
                                let _ = results.send((connection, message)).await;
                            });
                        }
                        JSONRPCMessage::Notification(notification) => {
                            if notification.method == "initialized" {
                                for request in pending.values() { send(&mut ui, &Audience::Connection(connection), request); }
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
                                && client.request_handle().reject_server_request(error.id.clone(), error.error).await.is_ok()
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
                    send(&mut ui, &Audience::All, &notification);
                }
                Some(AppServerEvent::ServerRequest(request)) => {
                    if let Ok(value) = serde_json::to_value(&request)
                        && let Some(id) = value.get("id").and_then(|id| serde_json::from_value(id.clone()).ok()) {
                            pending.insert(id, value.clone());
                            send(&mut ui, &Audience::All, &value);
                        }
                }
                Some(AppServerEvent::Lagged { .. } | AppServerEvent::Disconnected { .. }) | None => {
                    ui.clear();
                    upstream = None;
                    pending.clear();
                }
            },
            Some((connection, message)) = replies.recv() => {
                send(&mut ui, &Audience::Connection(connection), &message);
            }

        }
    }
}

enum Audience {
    Connection(u64),
    All,
}

fn send(ui: &mut HashMap<u64, Connection>, audience: &Audience, value: &impl serde::Serialize) {
    let text = match serde_json::to_string(value) {
        Ok(text) => text,
        Err(error) => {
            tracing::warn!(%error, "Failed to serialize UI broker message");
            return;
        }
    };
    let message = Message::Text(text.into());
    ui.retain(|connection, peer| {
        if matches!(audience, Audience::Connection(target) if connection != target) {
            return true;
        }
        peer.writer.try_send(message.clone()).is_ok()
    });
}
