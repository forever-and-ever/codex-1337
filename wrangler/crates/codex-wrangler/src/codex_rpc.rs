use std::{
    io::{BufRead as _, BufReader, Write as _},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(6);

pub struct CodexRpc {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<std::io::Result<String>>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
}

impl CodexRpc {
    pub fn open(home: &Path) -> Result<Self> {
        let mut child = Command::new("codex")
            .args(["app-server", "--listen", "stdio://"])
            .env("CODEX_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn Codex app-server")?;
        let stdin = child.stdin.take().context("open Codex app-server input")?;
        let stdout = child
            .stdout
            .take()
            .context("open Codex app-server output")?;
        let (send, lines) = mpsc::channel();
        let reader = thread::Builder::new()
            .name("codex-wrangler-rpc".to_owned())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let terminal = line.is_err();
                    if send.send(line).is_err() || terminal {
                        break;
                    }
                }
            })
            .context("spawn Codex app-server reader")?;
        let mut rpc = Self {
            child,
            stdin: Some(stdin),
            lines,
            reader: Some(reader),
            next_id: 1,
        };
        let _initialized = rpc.request(
            "initialize",
            &json!({
                "clientInfo": {
                    "name": "codex_wrangler",
                    "title": "Codex Wrangler",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        rpc.notify("initialized", &json!({}))?;
        Ok(rpc)
    }

    pub fn rename_thread(&mut self, thread: &str, name: &str) -> Result<()> {
        let _renamed = self.request(
            "thread/name/set",
            &json!({ "threadId": thread, "name": name }),
        )?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.send(&json!({ "method": method, "id": id, "params": params }))?;
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(timeout)
                .with_context(|| format!("wait for Codex app-server `{method}` response"))??;
            let message: Value = serde_json::from_str(&line)
                .with_context(|| format!("decode Codex app-server line `{line}`"))?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                bail!("Codex app-server `{method}` rejected the request: {error}");
            }
            return message
                .get("result")
                .cloned()
                .with_context(|| format!("Codex app-server `{method}` omitted its result"));
        }
    }

    fn notify(&mut self, method: &str, params: &Value) -> Result<()> {
        self.send(&json!({ "method": method, "params": params }))
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .context("Codex app-server input closed")?;
        serde_json::to_writer(&mut *stdin, message).context("encode Codex app-server request")?;
        stdin.write_all(b"\n")?;
        stdin.flush().context("flush Codex app-server request")
    }
}

impl Drop for CodexRpc {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _killed = self.child.kill();
        let _waited = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _joined = reader.join();
        }
    }
}
