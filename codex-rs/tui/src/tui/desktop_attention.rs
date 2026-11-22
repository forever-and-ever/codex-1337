//! Optional local X11/i3 attention signals. Observer failure always restores a usable UI.

use std::collections::HashMap;
use std::os::fd::AsFd;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use codex_utils_path::system_executable;
use codex_utils_path::system_path;
use futures::FutureExt;
use futures::StreamExt;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::watch;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::sync::ConnectionExt as _;
use x11rb::protocol::sync::CreateAlarmAux;
use x11rb::protocol::sync::Int64;
use x11rb::protocol::sync::TESTTYPE;
use x11rb::protocol::sync::VALUETYPE;

use super::DesktopAttention;

const IDLE_THRESHOLD: Duration = Duration::from_secs(30);
const X11_INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(2);
const I3_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(500);
const I3_SNAPSHOT_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowAttention {
    visible: Option<bool>,
    focused: Option<bool>,
}

pub(crate) fn start() -> watch::Receiver<DesktopAttention> {
    let (tx, rx) = watch::channel(DesktopAttention::default());
    if std::env::var_os("DISPLAY").is_some() {
        let idle_tx = tx.clone();
        tokio::spawn(async move {
            if let Err(error) = observe_idle(idle_tx.clone()).await {
                tracing::debug!(%error, "X11 idle observer unavailable");
                idle_tx.send_modify(|state| state.idle = false);
            }
        });
    }
    let window_id = std::env::var("ALACRITTY_WINDOW_ID")
        .or_else(|_| std::env::var("WINDOWID"))
        .ok()
        .and_then(|id| {
            id.parse::<u64>()
                .or_else(|_| u64::from_str_radix(id.trim_start_matches("0x"), 16))
                .ok()
        });
    if let Some(window_id) = window_id {
        tokio::spawn(async move {
            if let Err(error) = observe_window_attention(tx.clone(), window_id).await {
                tracing::debug!(%error, "i3 window observer unavailable");
                tx.send_modify(|state| {
                    state.visible = None;
                    state.focused = None;
                });
            }
        });
    }
    rx
}

async fn observe_idle(tx: watch::Sender<DesktopAttention>) -> Result<()> {
    // x11rb's setup/reply APIs are synchronous and may wait forever for a broken X server.
    // Give setup one dedicated OS thread rather than a Tokio blocking-pool worker: a timed-out
    // X reply cannot then stall Tokio's runtime shutdown or block agent/terminal input.
    let (setup_tx, setup_rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("tui-x11-idle-setup".into())
        .spawn(move || {
            let result = initialize_idle_alarm();
            let _ = setup_tx.send(result);
        })?;
    let (conn, idle_millis) = tokio::select! {
        result = tokio::time::timeout(X11_INITIALIZATION_TIMEOUT, setup_rx) => {
            result.context("X11 idle initialization timed out")?
                .context("X11 idle initialization channel closed")??
        }
        () = tx.closed() => return Ok(()),
    };

    let idle = idle_millis.hi > 0 || idle_millis.lo >= 30_000;
    tx.send_if_modified(|state| {
        if state.idle == idle {
            return false;
        }
        state.idle = idle;
        true
    });
    // XSync alarms wake on both the idle crossing and the clock reset caused by human input.
    // The X11 socket itself is registered with Tokio; no periodic desktop query is needed.
    // SAFETY: The borrow ties the registration to the connection's fixed socket.
    let readable = unsafe { tokio::io::unix::AsyncFd::register(conn.stream().as_fd()) }
        .map_err(std::io::Error::from)?;
    loop {
        // A synchronous reply may already have pulled an alarm into x11rb's event queue;
        // socket readability alone would then never wake the observer again.
        drain_idle_alarms(&conn, &tx)?;
        let mut ready = tokio::select! {
            ready = readable.readable() => ready?,
            () = tx.closed() => return Ok(()),
        };
        drain_idle_alarms(&conn, &tx)?;
        ready.clear_ready();
    }
}

fn drain_idle_alarms(
    conn: &x11rb::rust_connection::RustConnection,
    tx: &watch::Sender<DesktopAttention>,
) -> Result<()> {
    while let Some(event) = conn.poll_for_event()? {
        if let Event::SyncAlarmNotify(event) = event {
            let elapsed = event.counter_value;
            let idle = elapsed.hi > 0 || elapsed.lo >= 30_000;
            tx.send_if_modified(|state| {
                if state.idle == idle {
                    return false;
                }
                state.idle = idle;
                true
            });
        }
    }
    Ok(())
}

fn initialize_idle_alarm() -> Result<(x11rb::rust_connection::RustConnection, Int64)> {
    let (conn, _) = x11rb::connect(None)?;
    conn.sync_initialize(
        /*desired_major_version*/ 3, /*desired_minor_version*/ 1,
    )?
    .reply()?;
    let counter = conn
        .sync_list_system_counters()?
        .reply()?
        .counters
        .into_iter()
        .find(|counter| counter.name.as_slice() == b"IDLETIME")
        .context("XSync IDLETIME counter unavailable")?
        .counter;
    let threshold = Int64 {
        hi: 0,
        lo: u32::try_from(IDLE_THRESHOLD.as_millis())
            .context("X11 idle threshold exceeds the protocol's millisecond range")?,
    };
    for test_type in [TESTTYPE::POSITIVE_TRANSITION, TESTTYPE::NEGATIVE_TRANSITION] {
        let alarm = conn.generate_id()?;
        conn.sync_create_alarm(
            alarm,
            &CreateAlarmAux::new()
                .counter(counter)
                .value_type(VALUETYPE::ABSOLUTE)
                .value(threshold)
                .test_type(test_type)
                .delta(Int64 { hi: 0, lo: 0 })
                .events(1),
        )?
        .check()?;
    }
    let elapsed = conn.sync_query_counter(counter)?.reply()?.counter_value;
    Ok((conn, elapsed))
}

async fn observe_window_attention(
    tx: watch::Sender<DesktopAttention>,
    window_id: u64,
) -> Result<()> {
    // i3-msg -m hides the subscription acknowledgement. Use IPC directly so the
    // initial snapshot cannot race subscription setup or wait for a desktop event.
    let socket = tokio::time::timeout(I3_SNAPSHOT_TIMEOUT, async {
        let path = if let Some(path) = std::env::var_os("I3SOCK") {
            std::path::PathBuf::from(path)
        } else {
            let output = Command::new(system_executable("i3").context("i3 is not installed")?)
                .env("PATH", system_path()?)
                .arg("--get-socketpath")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await?;
            ensure!(output.status.success(), "i3 socket discovery failed");
            std::path::PathBuf::from(std::str::from_utf8(&output.stdout)?.trim())
        };
        let mut socket = UnixStream::connect(path).await?;
        let payload = b"[\"window\",\"workspace\",\"output\"]";
        let mut request = b"i3-ipc".to_vec();
        request.extend_from_slice(
            &u32::try_from(payload.len())
                .context("i3 subscription exceeds the protocol message size")?
                .to_ne_bytes(),
        );
        request.extend_from_slice(&2_u32.to_ne_bytes()); // SUBSCRIBE
        request.extend_from_slice(payload);
        socket.write_all(&request).await?;
        let (kind, reply) = read_i3_message(&mut socket).await?;
        ensure!(
            kind == 2 && reply["success"] == true,
            "i3 subscription rejected"
        );
        Ok::<_, anyhow::Error>(socket)
    })
    .await
    .context("i3 subscription handshake timed out")??;
    // The stream retains partial reads across cancellation by select/now_or_never.
    let mut events = Box::pin(futures::stream::try_unfold(socket, |mut socket| async {
        let (_, event) = read_i3_message(&mut socket).await?;
        Ok::<_, anyhow::Error>(Some((event, socket)))
    }));
    update_window_attention(&tx, window_id).await;
    loop {
        tokio::select! {
            event = events.next() => {
                let Some(event) = event else {
                    anyhow::bail!("i3 subscription closed");
                };
                if !affects_window_attention(&event?) {
                    continue;
                }
                // Burst events describe one new tree. Do not fork a snapshot subprocess per
                // intermediate focus/move notification.
                tokio::time::sleep(Duration::from_millis(30)).await;
                while let Some(result) = events.next().now_or_never() {
                    result.context("i3 subscription closed")??;
                }
                update_window_attention(&tx, window_id).await;
            }
            () = tx.closed() => return Ok(()),
        }
    }
}

async fn read_i3_message(socket: &mut UnixStream) -> Result<(u32, Value)> {
    let mut header = [0_u8; 14];
    socket.read_exact(&mut header).await?;
    ensure!(&header[..6] == b"i3-ipc", "invalid i3 IPC header");
    let length = u32::from_ne_bytes(header[6..10].try_into()?);
    ensure!(
        u64::from(length) <= I3_SNAPSHOT_MAX_BYTES,
        "oversized i3 event"
    );
    let kind = u32::from_ne_bytes(header[10..14].try_into()?);
    let mut payload = vec![0; length as usize];
    socket.read_exact(&mut payload).await?;
    Ok((kind, serde_json::from_slice(&payload)?))
}

fn affects_window_attention(event: &Value) -> bool {
    // Terminal titles refresh frequently while an agent runs. Title, mark and urgency
    // notifications cannot move, occlude or reveal a window.
    !matches!(event["change"].as_str(), Some("title" | "mark" | "urgent"))
}

async fn update_window_attention(tx: &watch::Sender<DesktopAttention>, window_id: u64) {
    let attention = if let (Some(tree), Some(workspaces)) = (
        i3_snapshot("get_tree").await,
        i3_snapshot("get_workspaces").await,
    ) {
        let visible_workspaces: HashMap<_, _> = workspaces
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|workspace| {
                Some((workspace["id"].as_u64()?, workspace["visible"].as_bool()?))
            })
            .collect();
        window_attention(&tree, window_id, Some(true), &visible_workspaces)
    } else {
        None
    };
    tx.send_if_modified(|state| {
        let visible = attention.and_then(|attention| attention.visible);
        let focused = attention.and_then(|attention| attention.focused);
        if state.visible == visible && state.focused == focused {
            return false;
        }
        state.visible = visible;
        state.focused = focused;
        true
    });
}

async fn i3_snapshot(kind: &str) -> Option<Value> {
    let mut child = Command::new(system_executable("i3-msg")?)
        .env("PATH", system_path().ok()?)
        .args(["-t", kind])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut output = Vec::new();
    let stdout = child.stdout.take()?;
    let bytes = tokio::time::timeout(
        I3_SNAPSHOT_TIMEOUT,
        stdout
            .take(I3_SNAPSHOT_MAX_BYTES + 1)
            .read_to_end(&mut output),
    )
    .await
    .ok()?
    .ok()?;
    if bytes as u64 > I3_SNAPSHOT_MAX_BYTES {
        return None;
    }
    let status = tokio::time::timeout(I3_SNAPSHOT_TIMEOUT, child.wait())
        .await
        .ok()?
        .ok()?;
    (status.success())
        .then(|| serde_json::from_slice(&output).ok())
        .flatten()
}

fn window_attention(
    node: &Value,
    window_id: u64,
    parent_visible: Option<bool>,
    visible_workspaces: &HashMap<u64, bool>,
) -> Option<WindowAttention> {
    let visible = if node["type"] == "workspace" {
        if node["name"] == "__i3_scratch" {
            Some(false)
        } else {
            node["id"]
                .as_u64()
                .and_then(|id| visible_workspaces.get(&id).copied())
        }
    } else {
        parent_visible
    };
    if node["window"].as_u64() == Some(window_id) {
        return Some(WindowAttention {
            visible,
            focused: node["focused"].as_bool(),
        });
    }
    // A focused floating child leads i3's focus list without replacing the visible tab.
    let selected_tiled = node["nodes"].as_array().and_then(|children| {
        node["focus"].as_array().and_then(|focus| {
            focus
                .iter()
                .find(|id| children.iter().any(|child| child.get("id") == Some(*id)))
        })
    });
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node[key].as_array() {
            for child in children {
                let tab_hidden = key == "nodes"
                    && matches!(node["layout"].as_str(), Some("tabbed" | "stacked"))
                    && selected_tiled.is_some_and(|id| id != &child["id"]);
                let child_visible = if tab_hidden { Some(false) } else { visible };
                if let Some(attention) =
                    window_attention(child, window_id, child_visible, visible_workspaces)
                {
                    return Some(attention);
                }
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "desktop_attention_tests.rs"]
mod tests;
