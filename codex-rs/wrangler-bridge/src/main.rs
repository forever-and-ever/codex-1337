use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Parser;
use codex_app_server_protocol::InitializeResponse;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadActiveFlag;
use codex_app_server_protocol::ThreadListResponse;
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadSource;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_transport::app_server_control_socket_path;
use codex_uds::UnixStream;
use futures::SinkExt;
use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::Message;

const BRIDGE_PROTOCOL: u16 = 2;
const HISTORY_PERIOD: Duration = Duration::from_secs(30);
const PAGE_SIZE: u32 = 500;
const ROSTER_VERSION: u8 = 2;

#[derive(Debug, Parser)]
#[command(version, about = "Codex Wrangler's site-side protocol bridge")]
struct Cli {
    /// Emit the endpoint handshake and one complete snapshot, then exit.
    #[arg(long, conflicts_with = "watch")]
    snapshot: bool,

    /// Continue emitting live frames and periodic history frames.
    #[arg(long)]
    watch: bool,

    /// Live-state sampling period while watching.
    #[arg(long, default_value_t = 2_000)]
    interval_ms: u64,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RemoteWork {
    Error,
    Input,
    Turn,
    Done,
}

#[derive(Debug, Serialize)]
struct LiveThread {
    thread: String,
    name: Option<String>,
    cwd: String,
    preview: String,
    work: RemoteWork,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
struct HistoricalThread {
    thread: String,
    name: Option<String>,
    cwd: String,
    preview: String,
    updated_at: i64,
    last_turn: String,
    archived: bool,
}

#[derive(Debug, Deserialize)]
struct RosterState {
    version: u8,
    sessions: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "frame", rename_all = "kebab-case")]
enum Frame {
    Hello {
        protocol: u16,
        bridge_version: &'static str,
        codex_version: String,
        platform: String,
    },
    Live {
        threads: Vec<LiveThread>,
    },
    History {
        threads: Vec<HistoricalThread>,
    },
    Roster {
        threads: Vec<String>,
    },
}

struct Rpc {
    socket: WebSocketStream<UnixStream>,
    next_id: u64,
    initialized: InitializeResponse,
}

impl Rpc {
    async fn connect() -> Result<Self> {
        let home = codex_utils_home_dir::find_codex_home().context("resolve Codex home")?;
        let path = app_server_control_socket_path(&home).context("resolve app-server socket")?;
        let stream = UnixStream::connect(path.as_path())
            .await
            .with_context(|| format!("connect to shared app-server `{}`", path.display()))?;
        let (socket, _response) = client_async("ws://localhost/", stream)
            .await
            .context("upgrade shared app-server connection")?;
        let mut rpc = Self {
            socket,
            next_id: 1,
            initialized: InitializeResponse {
                user_agent: String::new(),
                codex_home: home,
                platform_family: String::new(),
                platform_os: String::new(),
            },
        };
        rpc.initialized = rpc
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "codex_wrangler_bridge",
                        "title": "Codex Wrangler Bridge",
                        "version": concat!(env!("CARGO_PKG_VERSION"), "-wrangler")
                    }
                }),
            )
            .await?;
        rpc.notify("initialized", json!({})).await?;
        Ok(rpc)
    }

    async fn request<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> Result<T> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.send(json!({ "method": method, "id": id, "params": params }))
            .await?;
        loop {
            let message = self.read().await?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(anyhow!(
                    "app-server `{method}` rejected the request: {error}"
                ));
            }
            let result = message
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow!("app-server `{method}` omitted its result"))?;
            return serde_json::from_value(result)
                .with_context(|| format!("decode app-server `{method}` response"));
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({ "method": method, "params": params }))
            .await
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        self.socket
            .send(Message::Text(serde_json::to_string(&value)?.into()))
            .await
            .context("write app-server request")
    }

    async fn read(&mut self) -> Result<Value> {
        loop {
            let frame = self
                .socket
                .next()
                .await
                .ok_or_else(|| anyhow!("shared app-server closed its socket"))??;
            if let Message::Text(payload) = frame {
                return serde_json::from_str(&payload).context("decode app-server message");
            }
        }
    }

    fn hello(&self) -> Frame {
        Frame::Hello {
            protocol: BRIDGE_PROTOCOL,
            bridge_version: concat!(env!("CARGO_PKG_VERSION"), "-wrangler"),
            codex_version: app_server_version(&self.initialized.user_agent),
            platform: self.initialized.platform_os.clone(),
        }
    }

    async fn live(&mut self) -> Result<Frame> {
        let loaded: ThreadLoadedListResponse = self
            .request(
                "thread/loaded/list",
                json!({ "limit": null, "cursor": null }),
            )
            .await?;
        let mut threads = Vec::with_capacity(loaded.data.len());
        for thread in loaded.data {
            let response: ThreadReadResponse = self
                .request(
                    "thread/read",
                    json!({ "threadId": thread, "includeTurns": false }),
                )
                .await?;
            if let Some(thread) = project_live(response.thread) {
                threads.push(thread);
            }
        }
        Ok(Frame::Live { threads })
    }

    async fn history(&mut self) -> Result<Frame> {
        let mut threads = self.history_partition(/*archived*/ false).await?;
        threads.extend(self.history_partition(/*archived*/ true).await?);
        threads.sort_unstable_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.thread.cmp(&right.thread))
        });
        Ok(Frame::History { threads })
    }

    async fn history_partition(&mut self, archived: bool) -> Result<Vec<HistoricalThread>> {
        let mut cursor = None;
        let mut threads = Vec::new();
        loop {
            let response: ThreadListResponse = self
                .request(
                    "thread/list",
                    json!({
                        "cursor": cursor,
                        "limit": PAGE_SIZE,
                        "sortKey": "updated_at",
                        "sortDirection": "desc",
                        "sourceKinds": ["cli", "vscode", "appServer"],
                        "archived": archived,
                        "useStateDbOnly": true
                    }),
                )
                .await?;
            threads.extend(response.data.into_iter().filter_map(|thread| {
                manual(&thread).then(|| HistoricalThread {
                    thread: thread.id,
                    name: thread.name,
                    cwd: thread.cwd.as_path().display().to_string(),
                    preview: thread.preview,
                    updated_at: thread.updated_at,
                    last_turn: chrono::DateTime::from_timestamp(thread.updated_at, 0).map_or_else(
                        || "UNKNOWN".to_owned(),
                        |timestamp| {
                            timestamp
                                .with_timezone(&chrono::Local)
                                .format("%Y-%m-%d %H:%M")
                                .to_string()
                        },
                    ),
                    archived,
                })
            }));
            let Some(next) = response.next_cursor else {
                break;
            };
            cursor = Some(next);
        }
        Ok(threads)
    }
}

fn project_live(thread: Thread) -> Option<LiveThread> {
    if !manual(&thread) {
        return None;
    }
    let work = match &thread.status {
        ThreadStatus::NotLoaded => return None,
        ThreadStatus::Idle => RemoteWork::Done,
        ThreadStatus::SystemError => RemoteWork::Error,
        ThreadStatus::Active { active_flags }
            if active_flags.iter().any(|flag| {
                matches!(
                    flag,
                    ThreadActiveFlag::WaitingOnApproval | ThreadActiveFlag::WaitingOnUserInput
                )
            }) =>
        {
            RemoteWork::Input
        }
        ThreadStatus::Active { .. } => RemoteWork::Turn,
    };
    Some(LiveThread {
        thread: thread.id,
        name: thread.name,
        cwd: thread.cwd.as_path().display().to_string(),
        preview: thread.preview,
        work,
        updated_at: thread.updated_at,
    })
}

fn manual(thread: &Thread) -> bool {
    thread.parent_thread_id.is_none()
        && thread.agent_role.is_none()
        && thread
            .thread_source
            .as_ref()
            .is_none_or(|source| matches!(source, ThreadSource::User))
}

fn app_server_version(user_agent: &str) -> String {
    user_agent
        .split_once('/')
        .and_then(|(_, suffix)| suffix.split_whitespace().next())
        .unwrap_or("unknown")
        .to_owned()
}

fn roster() -> Result<Frame> {
    let path = roster_path()?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(Frame::Roster {
                threads: Vec::new(),
            });
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read Wrangler roster `{}`", path.display()));
        }
    };
    let state = serde_json::from_slice::<RosterState>(&bytes)
        .with_context(|| format!("decode Wrangler roster `{}`", path.display()))?;
    anyhow::ensure!(
        state.version == ROSTER_VERSION,
        "Wrangler roster `{}` uses version {}, expected {ROSTER_VERSION}",
        path.display(),
        state.version
    );
    Ok(Frame::Roster {
        threads: state.sessions.into_keys().collect(),
    })
}

fn roster_path() -> Result<PathBuf> {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/state"))
        })
        .map(|root| root.join("codex-wrangler/known-sessions.json"))
        .context("neither absolute XDG_STATE_HOME nor HOME is available")
}

fn emit(frame: &Frame) -> Result<()> {
    serde_json::to_writer(std::io::stdout().lock(), frame)?;
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    anyhow::ensure!(cli.interval_ms > 0, "--interval-ms must be positive");
    let mut rpc = Rpc::connect().await?;
    emit(&rpc.hello())?;
    emit(&rpc.history().await?)?;
    emit(&rpc.live().await?)?;
    emit(&roster()?)?;
    if cli.snapshot || !cli.watch {
        return Ok(());
    }

    let mut live = tokio::time::interval(Duration::from_millis(cli.interval_ms));
    live.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut history = tokio::time::interval(HISTORY_PERIOD);
    history.set_missed_tick_behavior(MissedTickBehavior::Skip);
    live.tick().await;
    history.tick().await;
    loop {
        tokio::select! {
            _ = live.tick() => {
                emit(&rpc.live().await?)?;
                emit(&roster()?)?;
            },
            _ = history.tick() => emit(&rpc.history().await?)?,
        }
    }
}
