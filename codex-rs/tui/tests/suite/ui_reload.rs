//! Exercise the real CLI host, worker replacement, and recovery screen over a PTY.

const THREAD: &str = "00000000-0000-0000-0000-000000000001";

use super::focus_palette::PtyCodex;
use super::focus_palette::socket_home;
use super::focus_palette::write_test_config;
use anyhow::Result;
use anyhow::ensure;
use codex_app_server_protocol::JSONRPCMessage;
use futures::SinkExt;
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_reload_retains_steering_and_times_out_without_replaying_input() -> Result<()> {
    let root = codex_utils_cargo_bin::repo_root()?;
    let home = socket_home()?;
    write_test_config(home.path(), &root)?;
    let socket = home.path().join("server.sock");
    let listener = UnixListener::bind(&socket)?;
    let (blocked, gate) = watch::channel(false);
    let methods = Arc::new(Mutex::new(Vec::<(u64, String)>::new()));
    let seen = Arc::clone(&methods);
    let mut servers = tokio::task::JoinSet::new();
    let cwd = root.clone();
    servers.spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        let mut connection = 0;
        loop {
            let (stream, _) = listener.accept().await?;
            connection += 1;
            let methods = Arc::clone(&seen);
            let mut gate = gate.clone();
            let cwd = cwd.clone();
            connections.spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(stream).await?;
                let thread = json!({
                    "id": THREAD, "sessionId": THREAD, "preview": "", "ephemeral": false,
                    "modelProvider": "openai", "createdAt": 1, "updatedAt": 2,
                    "status": {"type": "active", "activeFlags": []}, "cwd": cwd,
                    "cliVersion": "0.0.0", "source": "cli",
                    "turns": [{"id": "running", "items": [], "status": "inProgress", "error": null}]
                });
                let mut pending = Vec::new();
                loop {
                    let request = tokio::select! {
                        changed = gate.changed() => {
                            if changed.is_err() { break }
                            if !*gate.borrow() {
                                for response in pending.drain(..) { ws.send(response).await?; }
                            }
                            continue;
                        }
                        frame = ws.next() => {
                            let Some(Ok(Message::Text(text))) = frame else { break };
                            let JSONRPCMessage::Request(request) = serde_json::from_str(&text)? else { continue };
                            request
                        }
                    };
                    methods.lock().unwrap().push((connection, request.method.clone()));
                    let result = match request.method.as_str() {
                        "initialize" => json!({"userAgent": "reload-pty"}),
                        "experimentalFeature/list" => json!({"data": (["code_mode_host", "auth_elicitation"].map(|name| json!({
                            "name": name, "stage": "stable", "displayName": null,
                            "description": null, "announcement": null,
                            "enabled": true, "defaultEnabled": true
                        }))), "nextCursor": null}),
                        "account/read" => json!({"account": {"type": "apiKey"}, "requiresOpenaiAuth": false}),
                        "model/list" => json!({"data": [], "nextCursor": null, "modelProvider": "openai"}),
                        "config/read" => json!({"config": {"model": "gpt-5.6-terra", "model_provider": "openai",
                            "tui": {"status_line": ["thread-id"]}, "projects": {
                                cwd.to_string_lossy(): {"trust_level": "trusted"}
                            }}, "origins": {}, "layers": []}),
                        "configRequirements/read" => json!({"requirements": null}),
                        "thread/start" | "thread/resume" => json!({"thread": thread, "model": "gpt-5.6-terra", "modelProvider": "openai",
                            "cwd": cwd, "approvalPolicy": "never", "approvalsReviewer": "user",
                            "sandbox": {"type": "readOnly"}, "reasoningEffort": null}),
                        "thread/read" => json!({"thread": thread}),
                        "thread/goal/get" => json!({"goal": null}),
                        "turn/steer" => json!({"turnId": "running"}),
                        "skills/list" => json!({"data": []}),
                        _ => {
                            ws.send(Message::Text(json!({"id": request.id, "error": {
                                "code": -32601, "message": "method not found"
                            }}).to_string().into())).await?;
                            continue;
                        }
                    };
                    let response = Message::Text(json!({"id": request.id, "result": result}).to_string().into());
                    if request.method == "thread/resume" && *gate.borrow() { pending.push(response); }
                    else { ws.send(response).await?; }
                }
                Ok::<_, anyhow::Error>(())
            });
        }
        #[expect(unreachable_code, reason = "JoinSet task requires an explicit Result output after its perpetual accept loop")]
        Ok::<_, anyhow::Error>(())
    });
    let endpoint = format!("unix://{}", socket.display());
    let mut terminal = PtyCodex::start_cli(
        &root,
        home,
        &[
            "--remote",
            &endpoint,
            "-c",
            "tui.disable_paste_burst=true",
            "resume",
            THREAD,
        ],
    )?;
    terminal.wait_for_startup()?;
    terminal.wait_for_screen("Working")?;
    terminal.write_input(b"retain-this-steering")?;
    terminal.read_output(Duration::from_millis(/*millis*/ 100))?;
    terminal.write_input(b"\r")?;
    terminal.wait_for_screen("retain-this-steering")?;
    terminal.write_input(b"future-followup\t")?;
    terminal.wait_for_screen("future-followup")?;
    terminal.write_input(b"saved-cut-buffer\x15")?;
    terminal.write_input(b"/reload")?;
    terminal.read_output(Duration::from_millis(/*millis*/ 100))?;
    terminal.write_input(b"\r")?;
    let until = Instant::now() + Duration::from_secs(/*secs*/ 8);
    while methods
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, method)| method == "thread/resume")
        .count()
        < 2
        && Instant::now() < until
    {
        terminal.read_output(Duration::from_millis(/*millis*/ 20))?;
    }
    let resumed = methods
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, method)| method == "thread/resume")
        .count()
        >= 2;
    ensure!(
        resumed,
        "reload never resumed: {:?}; screen: {}",
        methods.lock().unwrap(),
        terminal.screen_contents()
    );
    terminal.read_output(Duration::from_millis(/*millis*/ 500))?;
    terminal.wait_for_screen("retain-this-steering")?;
    terminal.wait_for_screen("future-followup")?;
    terminal.write_input(b"\x19")?;
    terminal.wait_for_screen("saved-cut-buffer")?;
    terminal.write_input(b"\x15")?;
    blocked.send(true)?;
    terminal.write_input(b"/reload")?;
    terminal.read_output(Duration::from_millis(/*millis*/ 100))?;
    terminal.write_input(b"\r")?;
    let start = Instant::now();
    while !terminal.screen_contains("UI reload failed")
        && start.elapsed() < Duration::from_secs(/*secs*/ 7)
    {
        terminal.read_output(Duration::from_millis(/*millis*/ 20))?;
    }
    ensure!(
        terminal.screen_contains("UI reload failed"),
        "reload did not reach recovery: {}",
        terminal.screen_contents()
    );
    let recovery_screen = terminal.screen_contents();
    blocked.send(false)?;
    terminal.write_input(b"b")?;
    terminal.wait_for_screen("retain-this-steering")?;
    terminal.wait_for_screen("future-followup")?;
    terminal.write_input(b"\x19")?;
    terminal.wait_for_screen("saved-cut-buffer")?;
    let methods = methods.lock().unwrap();
    ensure!(
        methods
            .iter()
            .filter(|(_, method)| method == "turn/steer")
            .count()
            == 1,
        "steering was lost or resent: {methods:?}"
    );
    ensure!(
        !methods
            .iter()
            .any(|(_, method)| method == "turn/start" || method == "turn/interrupt"),
        "reload altered server execution: {methods:?}"
    );
    let sessions = methods
        .iter()
        .filter(|(_, method)| matches!(method.as_str(), "thread/start" | "thread/resume"))
        .map(|(connection, _)| connection)
        .collect::<std::collections::HashSet<_>>();
    ensure!(
        sessions.len() == 1,
        "reload replaced the server connection: {methods:?}"
    );
    drop(methods);
    terminal.exit_with_input(b"\x15\x04")?;
    insta::assert_snapshot!("ui_reload_failed", recovery_screen);
    Ok(())
}
