use codex_app_server_protocol::AlarmSummary;
use codex_app_server_protocol::ThreadAlarmDisarmResponse;
use codex_app_server_protocol::ThreadArchiveResponse;
use codex_app_server_protocol::ThreadClosedNotification;
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadUnsubscribeResponse;
use codex_app_server_protocol::ThreadUnsubscribeStatus;
use core_test_support::mock_server;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadAlarmsUpdatedNotification;
use codex_app_server_protocol::ThreadBackgroundTerminalsListResponse;
use codex_app_server_protocol::ThreadBackgroundTerminalsTerminateResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_app_server_protocol::alarm_sounding::AlarmCause;
use codex_app_server_protocol::alarm_sounding::AlarmSounding;
use codex_features::Feature;
use codex_protocol::ThreadId;
use codex_protocol::models::ResponseItem;
use codex_state::DirectionalThreadSpawnEdgeStatus;
use codex_uds::UnixStream;
use core_test_support::responses;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::process::Child;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

use super::daemon_update_recovery::connect_daemon_client;
use super::daemon_update_recovery::request_shutdown;
use super::daemon_update_recovery::spawn_server;
use super::daemon_update_recovery::wait_success;

const READ_TIMEOUT: Duration = Duration::from_secs(30);

const READY_COMMAND: &str = "printf 'warming\\n'; sleep 3; printf 'ready\\n'; sleep 30";

struct AlarmApp {
    server: Child,
    socket: PathBuf,
    client: WebSocketStream<UnixStream>,
    pending: VecDeque<JSONRPCNotification>,
    next_request: i64,
}

impl AlarmApp {
    async fn new(home: &Path) -> Result<Self> {
        let socket = home.join("control/server.sock");
        let server = spawn_server(home, &socket)?;
        let client = connect_daemon_client(
            &socket,
            InitializeCapabilities {
                experimental_api: true,
                ..Default::default()
            },
        )
        .await?;
        Ok(Self {
            server,
            socket,
            client,
            pending: VecDeque::new(),
            next_request: 2,
        })
    }

    async fn message(&mut self) -> Result<JSONRPCMessage> {
        loop {
            let frame = self.client.next().await.context("daemon socket closed")??;
            if let Message::Text(text) = frame {
                return Ok(serde_json::from_str(&text)?);
            }
        }
    }

    async fn request<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> Result<T> {
        let id = self.next_request;
        self.next_request += 1;
        self.client
            .send(Message::Text(
                json!({"id": id, "method": method, "params": params})
                    .to_string()
                    .into(),
            ))
            .await?;
        timeout(READ_TIMEOUT, async {
            loop {
                match self.message().await? {
                    JSONRPCMessage::Response(response) if response.id == RequestId::Integer(id) => {
                        return Ok(serde_json::from_value(response.result)?);
                    }
                    JSONRPCMessage::Error(error) => {
                        anyhow::bail!("{method}: {}", error.error.message)
                    }
                    JSONRPCMessage::Notification(notification) => {
                        self.pending.push_back(notification);
                    }
                    message => anyhow::bail!("unexpected daemon message: {message:?}"),
                }
            }
        })
        .await?
    }

    async fn read_notification<T: DeserializeOwned>(&mut self, method: &str) -> Result<T> {
        timeout(READ_TIMEOUT, async {
            loop {
                if let Some(index) = self
                    .pending
                    .iter()
                    .position(|message| message.method == method)
                {
                    let notification = self.pending.remove(index).expect("notification index");
                    return Ok(serde_json::from_value(
                        notification.params.context("notification params")?,
                    )?);
                }
                match self.message().await? {
                    JSONRPCMessage::Notification(notification) => {
                        self.pending.push_back(notification);
                    }
                    message => anyhow::bail!("unexpected daemon message: {message:?}"),
                }
            }
        })
        .await?
    }

    async fn shutdown(&mut self) -> Result<()> {
        request_shutdown(&self.server, &self.socket).await?;
        wait_success(&mut self.server).await
    }
}

fn alarm_call(call_id: &str, arguments: &Value) -> Value {
    responses::ev_function_call_with_namespace(call_id, "alarm", "set", &arguments.to_string())
}

fn response(id: &str, items: Vec<Value>) -> String {
    responses::sse(
        [responses::ev_response_created(id)]
            .into_iter()
            .chain(items)
            .chain([responses::ev_completed(id)])
            .collect(),
    )
}

fn done(id: &str) -> String {
    response(id, vec![responses::ev_assistant_message(id, "Done")])
}

async fn alarm_app(server: &MockServer) -> Result<(AlarmApp, TempDir)> {
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_sandbox_mode("danger-full-access")
        .with_root_config("thread_unload_delay_secs = 1")
        .enable_feature(Feature::UnifiedExec)
        .disable_feature(Feature::ShellZshFork)
        .disable_feature(Feature::ShellSnapshot)
        .write(home.path())?;
    let app = AlarmApp::new(home.path()).await?;
    Ok((app, home))
}

async fn start(app: &mut AlarmApp) -> Result<String> {
    let started: ThreadStartResponse = app
        .request(
            "thread/start",
            serde_json::to_value(ThreadStartParams::default())?,
        )
        .await?;
    Ok(started.thread.id)
}

async fn arm_turn(app: &mut AlarmApp, thread_id: &str) -> Result<()> {
    let started: TurnStartResponse = app
        .request(
            "turn/start",
            serde_json::to_value(TurnStartParams {
                thread_id: thread_id.to_string(),
                input: vec![UserInput::Text {
                    text: "Arm the alarms, then end this turn.".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            })?,
        )
        .await?;
    let completed: TurnCompletedNotification = app.read_notification("turn/completed").await?;
    assert_eq!(completed.turn.id, started.turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    Ok(())
}

async fn alarms(app: &mut AlarmApp, count: usize) -> Result<ThreadAlarmsUpdatedNotification> {
    timeout(READ_TIMEOUT, async {
        loop {
            let updated: ThreadAlarmsUpdatedNotification =
                app.read_notification("thread/alarms/updated").await?;
            if updated.alarms.len() == count {
                return Ok(updated);
            }
        }
    })
    .await?
}

async fn soundings(server: &MockServer) -> Result<Vec<AlarmSounding>> {
    let mut soundings = Vec::new();
    for request in server
        .received_requests()
        .await
        .context("captured requests")?
    {
        if !request.url.path().ends_with("/responses") {
            continue;
        }
        let body: Value = request.body_json()?;
        for item in body["input"].as_array().context("Responses input")? {
            let Ok(ResponseItem::FunctionCallOutput {
                call_id: None,
                name: Some(name),
                namespace,
                output,
                ..
            }) = serde_json::from_value(item.clone())
            else {
                continue;
            };
            if let Some(sounding) =
                AlarmSounding::from_tool_output(&name, namespace.as_deref(), &output.body)
                && !soundings
                    .iter()
                    .any(|seen: &AlarmSounding| seen.id == sounding.id)
            {
                assert!(
                    item.get("call_id").is_none(),
                    "unsolicited sounding has no call ID"
                );
                soundings.push(sounding);
            }
        }
    }
    Ok(soundings)
}

async fn wait_for_soundings(
    app: &mut AlarmApp,
    server: &MockServer,
    count: usize,
) -> Result<Vec<AlarmSounding>> {
    timeout(READ_TIMEOUT, async {
        loop {
            let completed: TurnCompletedNotification =
                app.read_notification("turn/completed").await?;
            assert_eq!(completed.turn.status, TurnStatus::Completed);
            let soundings = soundings(server).await?;
            if soundings.len() == count {
                return Ok(soundings);
            }
        }
    })
    .await?
}

#[tokio::test]
async fn clock_alarm_starts_a_turn_without_client_submission() -> Result<()> {
    let server = responses::start_mock_server().await;
    mock_server::run(&[&server], async {
        let model = responses::mount_sse_sequence(
            &server,
            vec![
                response(
                    "arm",
                    vec![
                        alarm_call(
                            "clock",
                            &json!({"after_seconds": 1, "note": "return to the work"}),
                        ),
                        alarm_call(
                            "future",
                            &json!({"after_seconds": 30, "note": "discarded by archive"}),
                        ),
                    ],
                ),
                done("armed"),
                done("sounded"),
            ],
        )
        .await;
        let (mut app, _home) = alarm_app(&server).await?;
        let thread_id = start(&mut app).await?;
        arm_turn(&mut app, &thread_id).await?;
        assert_eq!(alarms(&mut app, 2).await?.thread_id, thread_id);
        let soundings = wait_for_soundings(&mut app, &server, 1).await?;
        assert_eq!(soundings[0].note, "return to the work");
        assert_eq!(
            soundings[0].cause,
            AlarmCause::Clock {
                late_by_seconds: None
            }
        );
        let armed = alarms(&mut app, 1).await?;
        assert_eq!(soundings[0].still_armed, vec![armed.alarms[0].id.clone()]);
        // Archive holds alarm mutation while stopping the runtime. Binding detach
        // must not wait reentrantly for that permit, and must discard durable rows.
        let _: ThreadArchiveResponse = app
            .request("thread/archive", json!({"threadId": thread_id}))
            .await?;
        assert_eq!(alarms(&mut app, 0).await?.thread_id, thread_id);
        assert_eq!(model.requests().len(), 3);
        Ok(())
    })
    .await
}

enum TerminalScenario {
    Ready,
    Restart,
}

// The model must arm the session actually returned by unified exec, not an OS PID
// or a guessed session number. Keep all four model steps behind this responder.
async fn terminal_model(server: &MockServer, scenario: TerminalScenario) {
    let command = match scenario {
        TerminalScenario::Ready => READY_COMMAND,
        TerminalScenario::Restart => "sleep 30",
    };
    let step = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(move |request: &wiremock::Request| {
            let body = match step.fetch_add(1, Ordering::SeqCst) {
                0 => response("exec", vec![responses::ev_exec_command_call_with_args("terminal", &json!({"cmd": command, "yield_time_ms": 1}))]),
                1 => {
                    let request: Value = request.body_json().expect("Responses body");
                    let item = request["input"].as_array().expect("Responses input").iter().find(|item| item["call_id"] == "terminal" && item["type"] == "function_call_output").expect("native exec output");
                    let output = match serde_json::from_value(item.clone()).expect("exec output item") { ResponseItem::FunctionCallOutput { output, .. } => Some(output), _ => None }.expect("function output");
                    let output = output.body.to_text().expect("text exec output");
                    let session_id: i32 = output.lines().find_map(|line| line.strip_prefix("Running (session ").and_then(|session| session.strip_suffix(')'))).expect("background session ID").parse().expect("numeric session ID");
                    let items = if matches!(scenario, TerminalScenario::Restart) {
                        vec![alarm_call("watch", &json!({"session_id": session_id, "note": "watch survives as lost"})), alarm_call("clock", &json!({"after_seconds": 3, "note": "clock survives restart"})), alarm_call("future", &json!({"after_seconds": 3600, "note": "future clock pins recovered runtime"}))]
                    } else {
                        assert!(output.contains("warming"), "expected initial native output: {output}");
                        vec![alarm_call("watch", &json!({"session_id": session_id, "pattern": "^ready$", "note": "terminal is ready"}))]
                    };
                    response("arm", items)
                }
                _ => done("done"),
            };
            ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(body)
        })
        .mount(server)
        .await;
}

#[tokio::test]
async fn terminal_alarm_matches_a_line_after_the_arming_turn_ends() -> Result<()> {
    let server = responses::start_mock_server().await;
    terminal_model(&server, TerminalScenario::Ready).await;
    let (mut app, home) = alarm_app(&server).await?;
    let selected = start(&mut app).await?;
    let middle = start(&mut app).await?;
    let unrelated = start(&mut app).await?;
    let thread_id = start(&mut app).await?;
    let config = core_test_support::load_default_config_for_test(&home).await;
    let db =
        codex_state::StateRuntime::init(config.sqlite.clone(), config.model_provider_id.clone())
            .await?;
    // Persisted family membership, not frontend attachment, determines which
    // live sessions belong to an ancestor's inventory.
    db.upsert_thread_spawn_edge(
        ThreadId::from_string(&selected)?,
        ThreadId::from_string(&middle)?,
        DirectionalThreadSpawnEdgeStatus::Open,
    )
    .await?;
    db.upsert_thread_spawn_edge(
        ThreadId::from_string(&middle)?,
        ThreadId::from_string(&thread_id)?,
        DirectionalThreadSpawnEdgeStatus::Open,
    )
    .await?;
    db.close().await;
    arm_turn(&mut app, &thread_id).await?;
    let armed = alarms(&mut app, 1).await?;
    let unsubscribed: ThreadUnsubscribeResponse = app
        .request("thread/unsubscribe", json!({"threadId": thread_id}))
        .await?;
    assert_eq!(unsubscribed.status, ThreadUnsubscribeStatus::Unsubscribed);
    // The armed watch must survive without a subscriber past the unload delay.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let loaded: ThreadLoadedListResponse = app.request("thread/loaded/list", json!({})).await?;
    assert!(loaded.data.contains(&thread_id));
    let soundings = timeout(READ_TIMEOUT, async {
        loop {
            let captured = soundings(&server).await?;
            if captured.len() == 1 {
                return Ok::<_, anyhow::Error>(captured);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert_eq!(soundings[0].id, armed.alarms[0].id);
    assert_eq!(soundings[0].note, "terminal is ready");
    let AlarmCause::Matched { command, line, .. } = &soundings[0].cause else {
        anyhow::bail!("expected matched sounding: {:?}", soundings[0])
    };
    assert_eq!((command.as_str(), line.as_str()), (READY_COMMAND, "ready"));
    assert_eq!(soundings[0].still_armed, Vec::<String>::new());
    assert_eq!(alarms(&mut app, 0).await?.thread_id, thread_id);

    let local: ThreadBackgroundTerminalsListResponse = app
        .request(
            "thread/backgroundTerminals/list",
            json!({"threadId": selected}),
        )
        .await?;
    assert!(
        local.data.is_empty(),
        "the default inventory remains per-thread"
    );
    let subtree: ThreadBackgroundTerminalsListResponse = app
        .request(
            "thread/backgroundTerminals/list",
            json!({"threadId": selected, "scope": "subtree"}),
        )
        .await?;
    assert_eq!(subtree.data.len(), 1);
    let session = &subtree.data[0];
    assert_eq!(session.thread_id, thread_id);
    assert_eq!(session.command, READY_COMMAND);
    let direct: ThreadBackgroundTerminalsListResponse = app
        .request(
            "thread/backgroundTerminals/list",
            json!({"threadId": thread_id}),
        )
        .await?;
    assert_eq!(direct, subtree);
    let isolated: ThreadBackgroundTerminalsListResponse = app
        .request(
            "thread/backgroundTerminals/list",
            json!({"threadId": unrelated, "scope": "subtree"}),
        )
        .await?;
    assert_eq!(
        isolated.data,
        Vec::<codex_app_server_protocol::ThreadBackgroundTerminal>::new()
    );
    let stopped: ThreadBackgroundTerminalsTerminateResponse = app
        .request(
            "thread/backgroundTerminals/terminate",
            json!({"threadId": session.thread_id, "processId": session.process_id}),
        )
        .await?;
    assert!(stopped.terminated);
    let remaining: ThreadBackgroundTerminalsListResponse = app
        .request(
            "thread/backgroundTerminals/list",
            json!({"threadId": selected, "scope": "subtree"}),
        )
        .await?;
    assert_eq!(
        remaining.data,
        Vec::<codex_app_server_protocol::ThreadBackgroundTerminal>::new()
    );
    // Neither the sounded watch nor its former WatchTask retains a runtime lease.
    let closed: ThreadClosedNotification = app.read_notification("thread/closed").await?;
    assert_eq!(closed.thread_id, thread_id);
    Ok(())
}

#[tokio::test]
async fn restart_delivers_overdue_clock_and_lost_terminal_without_resuming_from_client()
-> Result<()> {
    let server = responses::start_mock_server().await;
    terminal_model(&server, TerminalScenario::Restart).await;
    let (mut app, home) = alarm_app(&server).await?;
    let thread_id = start(&mut app).await?;
    arm_turn(&mut app, &thread_id).await?;
    let armed = alarms(&mut app, 3).await?;
    app.shutdown().await?;
    drop(app);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let socket = home.path().join("control/server.sock");
    let successor = spawn_server(home.path(), &socket)?;
    // Recovery runs without a frontend subscription and may precede initialization.
    // Observe delivery at the model boundary rather than waiting for UI events.
    let recovered = timeout(READ_TIMEOUT, async {
        loop {
            let captured = soundings(&server).await?;
            if captured.len() == 2 {
                return Ok::<_, anyhow::Error>(captured);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert_eq!(recovered.len(), 2);
    let client = connect_daemon_client(
        &socket,
        InitializeCapabilities {
            experimental_api: true,
            ..Default::default()
        },
    )
    .await?;
    let mut app = AlarmApp {
        server: successor,
        socket,
        client,
        pending: VecDeque::new(),
        next_request: 2,
    };
    // Recovery delivery loads the root without subscribing this observer. The future
    // durable clock must keep that exact runtime loaded past its ordinary idle delay.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let loaded: ThreadLoadedListResponse = app.request("thread/loaded/list", json!({})).await?;
    assert!(loaded.data.contains(&thread_id));
    let future = armed
        .alarms
        .iter()
        .find(|alarm| alarm.note == "future clock pins recovered runtime")
        .context("future recovered clock")?;
    let soundings = recovered;
    let mut recovered_ids = soundings
        .iter()
        .map(|sounding| sounding.id.as_str())
        .collect::<Vec<_>>();
    let mut armed_ids = armed
        .alarms
        .iter()
        .filter(|alarm| alarm.id != future.id)
        .map(|alarm| alarm.id.as_str())
        .collect::<Vec<_>>();
    recovered_ids.sort_unstable();
    armed_ids.sort_unstable();
    assert_eq!(recovered_ids, armed_ids);
    let clock = soundings
        .iter()
        .find(|sounding| sounding.note == "clock survives restart")
        .context("recovered clock")?;
    assert_eq!(
        clock.cause,
        AlarmCause::Clock {
            late_by_seconds: None
        }
    );
    let watch = soundings
        .iter()
        .find(|sounding| sounding.note == "watch survives as lost")
        .context("recovered watch")?;
    assert!(matches!(&watch.cause, AlarmCause::Lost { command, .. } if command == "sleep 30"));
    let disarmed: ThreadAlarmDisarmResponse = app
        .request(
            "thread/alarm/disarm",
            json!({"threadId": thread_id, "id": future.id}),
        )
        .await?;
    assert_eq!(disarmed.alarms, Vec::<AlarmSummary>::new());
    let closed: ThreadClosedNotification = app.read_notification("thread/closed").await?;
    assert_eq!(closed.thread_id, thread_id);
    let loaded: ThreadLoadedListResponse = app.request("thread/loaded/list", json!({})).await?;
    assert!(!loaded.data.contains(&thread_id));
    app.shutdown().await?;
    Ok(())
}
