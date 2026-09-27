//! Private, bounded local control messages; never part of the app-server protocol.

use codex_app_server_client::RemoteAppServerEndpoint;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::io;
use std::path::Path;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

const MAX_MESSAGE: usize = 32 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub(super) enum Endpoint {
    Unix(AbsolutePathBuf),
    WebSocket { url: String, token: Option<String> },
}

impl From<RemoteAppServerEndpoint> for Endpoint {
    fn from(endpoint: RemoteAppServerEndpoint) -> Self {
        match endpoint {
            RemoteAppServerEndpoint::UnixSocket { socket_path } => Self::Unix(socket_path),
            RemoteAppServerEndpoint::WebSocket {
                websocket_url,
                auth_token,
            } => Self::WebSocket {
                url: websocket_url,
                token: auth_token,
            },
        }
    }
}

impl From<Endpoint> for RemoteAppServerEndpoint {
    fn from(endpoint: Endpoint) -> Self {
        match endpoint {
            Endpoint::Unix(socket_path) => Self::UnixSocket { socket_path },
            Endpoint::WebSocket {
                url: websocket_url,
                token: auth_token,
            } => Self::WebSocket {
                websocket_url,
                auth_token,
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct Envelope {
    pub(super) generation: u64,
    pub(super) request: Request,
}

#[derive(Serialize, Deserialize)]
pub(super) enum Request {
    Hello {
        protocol: String,
    },
    Connect(Endpoint),
    Reload(Value),
    Ready,
    Suspend,
    Tools {
        params: Box<codex_app_server_protocol::ThreadStartParams>,
    },
    Tasks {
        after: u64,
    },
}

#[derive(Serialize, Deserialize)]
pub(super) enum Response {
    State(Option<Value>),
    Ok,
    Error(String),
    Tools(Value),
    Tasks {
        cursor: u64,
        tasks: Vec<(codex_app_server_protocol::Thread, bool)>,
    },
}

pub(super) struct Received {
    pub(super) envelope: Envelope,
    pub(super) reply: oneshot::Sender<Response>,
}

pub(super) async fn listen(
    listener: UnixListener,
    events: mpsc::Sender<Received>,
) -> io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let events = events.clone();
        tokio::spawn(async move {
            let Ok(Ok(envelope)) = tokio::time::timeout(
                std::time::Duration::from_secs(/*secs*/ 10),
                read::<Envelope>(&mut stream),
            )
            .await
            else {
                return;
            };
            let subscription = matches!(envelope.request, Request::Suspend | Request::Tasks { .. });
            let operation = async {
                let (reply, response) = oneshot::channel();
                events
                    .send(Received { envelope, reply })
                    .await
                    .map_err(io::Error::other)?;
                let response = response.await.map_err(io::Error::other)?;
                write(&mut stream, &response).await
            };
            if subscription {
                // Explicit suspension and a background event subscription are not
                // reload waits. Keep idle hosts asleep instead of polling for tasks.
                let _ = operation.await;
            } else {
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 10), operation)
                        .await;
            }
        });
    }
}

pub(super) async fn request(directory: &Path, envelope: Envelope) -> io::Result<Response> {
    let mut stream = UnixStream::connect(directory.join("control.sock")).await?;
    write(&mut stream, &envelope).await?;
    read(&mut stream).await
}

/// Job control is synchronous: the renderer deliberately waits until the shell
/// resumes its host. This is not a reload wait and is never admitted during reload.
pub(super) fn suspend(directory: &Path, generation: u64) -> io::Result<()> {
    use std::io::Read;
    use std::io::Write;
    let mut stream = std::os::unix::net::UnixStream::connect(directory.join("control.sock"))?;
    let bytes = serde_json::to_vec(&Envelope {
        generation,
        request: Request::Suspend,
    })?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_MESSAGE {
        return Err(io::Error::other("UI host message too large"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    match serde_json::from_slice(&bytes)? {
        Response::Ok => Ok(()),
        Response::Error(message) => Err(io::Error::other(message)),
        _ => Err(io::Error::other("Unexpected suspend response")),
    }
}

async fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let len = stream.read_u32().await? as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UI host message too large",
        ));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

async fn write(stream: &mut UnixStream, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UI host message too large",
        ));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await
}
