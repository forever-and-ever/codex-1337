//! A real frontend connection is closed before the model may invoke coordination.
//! This catches capabilities accidentally hosted by a renderer or its transport.

use std::future::IntoFuture;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::Result;
use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::Response;
use axum::routing::any;
use axum::routing::post;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::delegation::DelegatedMessage;
use codex_utils_absolute_path::test_support::PathExt;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::time::Duration;
use tokio::time::timeout;

use super::connect_websocket;
use super::create_config_toml;
use super::read_jsonrpc_message;
use super::read_response_for_id;
use super::send_initialize_request;
use super::send_jsonrpc;
use super::send_request;
use super::spawn_websocket_server;
use super::start_thread;
use super::to_response;
use super::wait_for_loaded_threads;

const TIMEOUT: Duration = Duration::from_secs(30);

struct ModelGate {
    legacy_connections: AtomicUsize,
    legacy_calls: Mutex<Vec<String>>,
    blocked_model: Option<String>,
    blocked_entered: Notify,
    blocked_release: Notify,
    first: AtomicBool,
    entered: Notify,
    release: Notify,
    changed: Notify,
    calls: Mutex<Vec<Value>>,
    tools: Mutex<Vec<Value>>,
}

impl Default for ModelGate {
    fn default() -> Self {
        Self {
            legacy_connections: AtomicUsize::new(0),
            legacy_calls: Mutex::new(Vec::new()),
            blocked_model: None,
            blocked_entered: Notify::new(),
            blocked_release: Notify::new(),
            first: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
            changed: Notify::new(),
            calls: Mutex::new(Vec::new()),
            tools: Mutex::new(Vec::new()),
        }
    }
}

async fn retired_frontend_transport(State(gate): State<Arc<ModelGate>>) -> axum::http::StatusCode {
    gate.legacy_connections.fetch_add(1, Ordering::SeqCst);
    gate.changed.notify_one();
    axum::http::StatusCode::INTERNAL_SERVER_ERROR
}

fn frontend_transport_config(endpoint: &str) -> Value {
    json!({"url": endpoint, "http_headers": {"Authorization": format!("Bearer {}", uuid::Uuid::new_v4())},
    "default_tools_approval_mode": "approve", "tools": {
        "create_thread": {"approval_mode": "prompt"},
        "send_message_to_thread": {"approval_mode": "prompt"},
        "fork_thread": {"approval_mode": "prompt"}
    }})
}

// Age only authoritative persisted metadata: never rewrite model history or
// introduce a production clock seam. Both columns represent the same timestamp.
async fn age_destination(home: &std::path::Path, thread_id: &str) -> Result<i64> {
    let sqlite = codex_state::SqliteConfig::new_for_testing(home.abs());
    let pool = sqlite.open_read_write_pool(&sqlite.state_db_path()).await?;
    let updated_at = chrono::Utc::now().timestamp() - 48 * 60 * 60;
    timeout(TIMEOUT, async {
        loop {
            let changed =
                sqlx::query("UPDATE threads SET updated_at = ?, updated_at_ms = ? WHERE id = ?")
                    .bind(updated_at)
                    .bind(updated_at * 1_000)
                    .bind(thread_id)
                    .execute(&pool)
                    .await?
                    .rows_affected();
            if changed == 1 {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
        }
    })
    .await??;
    pool.close().await;
    Ok(updated_at)
}

// A never-used speculative thread has no durable rollout to restore. Seed cold
// fixtures through the public turn API and retain interleaved completion events.
async fn persist_destination(
    frontend: &mut super::WsClient,
    id: i64,
    thread_id: &str,
) -> Result<()> {
    send_request(
        frontend,
        "turn/start",
        id,
        Some(json!({
            "threadId": thread_id, "input": [{"type": "text", "text": "Persist this destination"}]
        })),
    )
    .await?;
    let mut started = false;
    let mut completed = false;
    timeout(TIMEOUT, async {
        while !started || !completed {
            match read_jsonrpc_message(frontend).await? {
                JSONRPCMessage::Response(response)
                    if response.id == codex_app_server_protocol::RequestId::Integer(id) =>
                {
                    started = true
                }
                JSONRPCMessage::Notification(notification)
                    if notification.method == "turn/completed"
                        && notification
                            .params
                            .as_ref()
                            .is_some_and(|params| params["threadId"] == thread_id) =>
                {
                    completed = true
                }
                JSONRPCMessage::Error(error) => {
                    anyhow::bail!("destination setup failed: {error:?}")
                }
                _ => {}
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

async fn model_response(State(gate): State<Arc<ModelGate>>, Json(body): Json<Value>) -> Response {
    let blocked =
        gate.blocked_model.as_deref() == body["model"].as_str() && gate.blocked_model.is_some();
    gate.calls.lock().await.push(body);
    gate.changed.notify_one();
    let first = !blocked && !gate.first.swap(true, Ordering::SeqCst);
    if first {
        gate.entered.notify_one();
    }
    if blocked {
        gate.blocked_entered.notify_one();
    }
    let stream = futures::stream::once(async move {
        if blocked {
            gate.blocked_release.notified().await;
        }
        let events = if first {
            gate.release.notified().await;
            let mut events = vec![responses::ev_response_created("coordination")];
            events.extend(gate.tools.lock().await.clone());
            events.push(responses::ev_completed("coordination"));
            events
        } else {
            vec![
                responses::ev_response_created("done"),
                responses::ev_assistant_message("answer", "done"),
                responses::ev_completed("done"),
            ]
        };
        Ok::<_, std::io::Error>(responses::sse(events))
    });
    Response::builder()
        .header(CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .expect("valid model fixture response")
}

#[path = "server_coordination_steering_tests.rs"]
mod steering;

#[path = "server_coordination_comms_tests.rs"]
mod comms;

#[path = "server_coordination_policy_tests.rs"]
mod policy;

#[tokio::test]
async fn independent_tasks_and_cold_destinations_start_after_the_only_frontend_disconnects()
-> Result<()> {
    let gate = Arc::new(ModelGate::default());
    gate.first.store(true, Ordering::SeqCst);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let mut legacy_transport = frontend_transport_config(&format!("http://{address}/mcp"));
    legacy_transport["enabled"] = json!(false);
    let model_server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/v1/responses", post(model_response))
                .route("/mcp", any(retired_frontend_transport))
                .with_state(Arc::clone(&gate)),
        )
        .into_future(),
    );
    let home = TempDir::new()?;
    create_config_toml(home.path(), &format!("http://{address}"), "never")?;
    let path = home.path().join("config.toml");
    let mut config = std::fs::read_to_string(&path)?;
    config.insert_str(0, "thread_unload_delay_secs = 0\n");
    config.push_str(
        r#"
[tools.thread_coordination.tools.send_message_to_thread]
approval_mode = "approve"
[tools.thread_coordination.tools.create_thread]
approval_mode = "approve"
"#,
    );
    std::fs::write(path, config)?;
    let (mut process, address) = spawn_websocket_server(home.path()).await?;
    let mut frontend = connect_websocket(address).await?;
    send_initialize_request(&mut frontend, /*id*/ 1, "sole-frontend").await?;
    read_response_for_id(&mut frontend, /*id*/ 1).await?;
    // Old listener registrations have no authentic ownership marker. Retire
    // transport explicitly; the native section independently owns user policy.
    // The live trap distinguishes retirement from tolerating a dead port.
    send_request(
        &mut frontend,
        "thread/start",
        /*id*/ 2,
        Some(json!({"config": {"mcp_servers.codex_tui": legacy_transport}})),
    )
    .await?;
    let caller =
        to_response::<ThreadStartResponse>(read_response_for_id(&mut frontend, /*id*/ 2).await?)?
            .thread
            .id;
    send_request(
        &mut frontend,
        "thread/name/set",
        /*id*/ 3,
        Some(json!({"threadId": caller, "name": "Dispatch & receive"})),
    )
    .await?;
    read_response_for_id(&mut frontend, /*id*/ 3).await?;
    send_request(
        &mut frontend,
        "thread/start",
        /*id*/ 4,
        Some(serde_json::to_value(ThreadStartParams {
            model: Some("destination-model".into()),
            ..Default::default()
        })?),
    )
    .await?;
    let destination =
        to_response::<ThreadStartResponse>(read_response_for_id(&mut frontend, /*id*/ 4).await?)?
            .thread
            .id;
    persist_destination(&mut frontend, /*id*/ 9, &destination).await?;
    send_request(
        &mut frontend,
        "thread/unsubscribe",
        /*id*/ 5,
        Some(json!({"threadId": destination})),
    )
    .await?;
    read_response_for_id(&mut frontend, /*id*/ 5).await?;
    wait_for_loaded_threads(&mut frontend, /*first_id*/ 100, &[&caller]).await?;

    let stale = start_thread(&mut frontend, /*id*/ 7).await?;
    persist_destination(&mut frontend, /*id*/ 10, &stale).await?;
    send_request(
        &mut frontend,
        "thread/unsubscribe",
        /*id*/ 8,
        Some(json!({"threadId": stale})),
    )
    .await?;
    read_response_for_id(&mut frontend, /*id*/ 8).await?;
    wait_for_loaded_threads(&mut frontend, /*first_id*/ 200, &[&caller]).await?;
    let stale_updated_at = age_destination(home.path(), &stale).await?;
    gate.calls.lock().await.clear();
    gate.first.store(false, Ordering::SeqCst);

    *gate.tools.lock().await = vec![
        responses::ev_function_call_with_namespace(
            "message",
            "mcp__codex_tui",
            "send_message_to_thread",
            &json!({"threadId": destination, "prompt": "Continue the cold task"}).to_string(),
        ),
        responses::ev_function_call_with_namespace(
            "new-task",
            "mcp__codex_tui",
            "create_thread",
            &json!({"prompt": "Start an independent task", "title": "Headless child"}).to_string(),
        ),
    ];
    gate.tools
        .lock()
        .await
        .push(responses::ev_function_call_with_namespace(
            "stale-message",
            "mcp__codex_tui",
            "send_message_to_thread",
            &json!({"threadId": stale, "prompt": "Do not revive a stale task"}).to_string(),
        ));
    send_request(
        &mut frontend,
        "turn/start",
        /*id*/ 6,
        Some(json!({"threadId": caller, "input": [{"type": "text", "text": "coordinate"}]})),
    )
    .await?;
    read_response_for_id(&mut frontend, /*id*/ 6).await?;
    timeout(TIMEOUT, gate.entered.notified()).await?;
    frontend.close(/*msg*/ None).await?;
    drop(frontend);
    // The response is held until the sole frontend is gone: no port, callbacks,
    // task registration acknowledgment, or renderer can execute either tool.
    gate.release.notify_one();
    let delivery = timeout(TIMEOUT, async {
        loop {
            if gate.calls.lock().await.len() >= 4 {
                break;
            }
            gate.changed.notified().await;
        }
    })
    .await;
    let calls = gate.calls.lock().await.clone();
    if delivery.is_err() {
        let summary = calls.iter().map(|body| json!({
            "model": body["model"],
            "outputs": body["input"].as_array().into_iter().flatten()
                .filter(|item| item["type"] == "function_call_output")
                .map(|item| json!({"callId": item["call_id"], "name": item["name"],
                    "output": item["output"].as_str().map(|text| text.chars().take(300).collect::<String>())}))
                .collect::<Vec<_>>()
        })).collect::<Vec<_>>();
        anyhow::bail!("headless coordination did not produce model requests: {summary:?}");
    }
    let delegated = |name: &str| {
        calls.iter().find_map(|body| {
            body["input"]
                .as_array()?
                .iter()
                .find(|item| item["type"] == "function_call_output" && item["name"] == name)
                .map(|item| (body, item))
        })
    };
    let (destination_request, message) =
        delegated("send_message_to_thread").context("cold destination request")?;
    assert_eq!(destination_request["model"], "destination-model");
    let message = DelegatedMessage::parse(message["output"].as_str().context("text envelope")?)
        .context("authentic delegation envelope")?;
    assert_eq!(
        (
            &message.source_thread_id,
            message.source_thread_name.as_deref(),
            &message.input
        ),
        (
            &caller,
            Some("Dispatch & receive"),
            &"Continue the cold task".to_string()
        )
    );
    assert!(message.sent_at.is_some());
    assert!(delegated("create_thread").is_some());
    assert_eq!(
        gate.legacy_connections.load(Ordering::SeqCst),
        0,
        "native coordination must not connect the retired frontend transport"
    );

    // A replacement frontend merely observes the authoritative runtime/index.
    let mut replacement = connect_websocket(address).await?;
    send_initialize_request(&mut replacement, /*id*/ 1, "replacement-frontend").await?;
    read_response_for_id(&mut replacement, /*id*/ 1).await?;
    send_request(
        &mut replacement,
        "thread/loaded/list",
        /*id*/ 10,
        Some(json!({})),
    )
    .await?;
    let loaded = read_response_for_id(&mut replacement, /*id*/ 10)
        .await?
        .result;
    assert!(
        !loaded["data"]
            .as_array()
            .context("loaded thread IDs")?
            .contains(&json!(stale)),
        "rejected messages must not restore a stale destination"
    );
    send_request(
        &mut replacement,
        "thread/read",
        /*id*/ 12,
        Some(json!({"threadId": stale, "includeTurns": false})),
    )
    .await?;
    let stale_view = to_response::<ThreadReadResponse>(
        read_response_for_id(&mut replacement, /*id*/ 12).await?,
    )?;
    assert_eq!(stale_view.thread.updated_at, stale_updated_at);
    let output = calls
        .iter()
        .flat_map(|body| body["input"].as_array().into_iter().flatten())
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "stale-message")
        .context("stale message rejection")?["output"]
        .as_str()
        .context("rejection text")?;
    assert!(
        output.contains("undeliverable") && output.contains("24 hours"),
        "{output}"
    );
    send_request(
        &mut replacement,
        "thread/resume",
        /*id*/ 2,
        Some(serde_json::to_value(ThreadResumeParams {
            thread_id: destination.clone(),
            exclude_turns: true,
            ..Default::default()
        })?),
    )
    .await?;
    read_response_for_id(&mut replacement, /*id*/ 2).await?;
    send_request(
        &mut replacement,
        "thread/read",
        /*id*/ 3,
        Some(serde_json::to_value(ThreadReadParams {
            thread_id: destination,
            include_turns: true,
        })?),
    )
    .await?;
    let history =
        to_response::<ThreadReadResponse>(read_response_for_id(&mut replacement, /*id*/ 3).await?)?;
    assert_eq!(history.thread.turns.len(), 2);
    assert_eq!(
        gate.calls.lock().await.len(),
        calls.len(),
        "reattaching must not replay coordination"
    );
    process.kill().await?;
    model_server.abort();
    Ok(())
}

#[tokio::test]
async fn headless_approval_waits_for_consent_and_replays_to_a_replacement_frontend() -> Result<()> {
    let gate = Arc::new(ModelGate::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let model_address = listener.local_addr()?;
    let model_server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/v1/responses", post(model_response))
                .with_state(Arc::clone(&gate)),
        )
        .into_future(),
    );
    let home = TempDir::new()?;
    create_config_toml(
        home.path(),
        &format!("http://{model_address}"),
        "on-request",
    )?;
    let path = home.path().join("config.toml");
    let mut config = std::fs::read_to_string(&path)?;
    config.push_str("\n[features]\ntool_call_mcp_elicitation = false\n");
    std::fs::write(path, config)?;
    let (mut process, address) = spawn_websocket_server(home.path()).await?;
    let mut frontend = connect_websocket(address).await?;
    send_initialize_request(&mut frontend, /*id*/ 1, "approval-origin").await?;
    read_response_for_id(&mut frontend, /*id*/ 1).await?;
    let caller = start_thread(&mut frontend, /*id*/ 2).await?;
    let destination = start_thread(&mut frontend, /*id*/ 3).await?;
    gate.first.store(true, Ordering::SeqCst);
    persist_destination(&mut frontend, /*id*/ 5, &destination).await?;
    gate.calls.lock().await.clear();
    gate.first.store(false, Ordering::SeqCst);
    *gate.tools.lock().await = vec![responses::ev_function_call_with_namespace(
        "pending-message",
        "mcp__codex_tui",
        "send_message_to_thread",
        &json!({"threadId": destination, "prompt": "Only after consent"}).to_string(),
    )];
    send_request(
        &mut frontend,
        "turn/start",
        /*id*/ 4,
        Some(json!({"threadId": caller, "input": [{"type": "text", "text": "coordinate"}]})),
    )
    .await?;
    read_response_for_id(&mut frontend, /*id*/ 4).await?;
    timeout(TIMEOUT, gate.entered.notified()).await?;
    frontend.close(None).await?;
    drop(frontend);
    gate.release.notify_one();

    let mut replacement = connect_websocket(address).await?;
    send_initialize_request(&mut replacement, /*id*/ 1, "approval-replacement").await?;
    // Requests are broadcast to initialized connections as well as replayed on
    // resume. Do not discard one that races the initialize response.
    let mut pending_approval = None;
    loop {
        match read_jsonrpc_message(&mut replacement)
            .await
            .context("initialize replacement while preserving pending consent")?
        {
            JSONRPCMessage::Response(response)
                if response.id == codex_app_server_protocol::RequestId::Integer(1) =>
            {
                break;
            }
            JSONRPCMessage::Request(request) if request.method == "item/tool/requestUserInput" => {
                pending_approval = Some(request)
            }
            JSONRPCMessage::Error(error) => anyhow::bail!("initialize failed: {error:?}"),
            _ => {}
        }
    }
    send_request(
        &mut replacement,
        "thread/resume",
        /*id*/ 2,
        Some(json!({"threadId": caller, "excludeTurns": true})),
    )
    .await?;
    // Resume and pending-request replay may interleave. Preserve both messages.
    let approval = timeout(TIMEOUT, async {
        let mut resumed = false;
        let mut approval = pending_approval;
        loop {
            match read_jsonrpc_message(&mut replacement)
                .await
                .context("resume caller and replay pending consent")?
            {
                JSONRPCMessage::Response(response)
                    if response.id == codex_app_server_protocol::RequestId::Integer(2) =>
                {
                    resumed = true
                }
                JSONRPCMessage::Request(request)
                    if request.method == "item/tool/requestUserInput" =>
                {
                    approval = Some(request)
                }
                JSONRPCMessage::Error(error) => anyhow::bail!("resume failed: {error:?}"),
                _ => {}
            }
            if resumed && let Some(approval) = approval.take() {
                break Ok::<_, anyhow::Error>(approval);
            }
        }
    })
    .await??;
    assert_eq!(
        gate.calls.lock().await.len(),
        1,
        "no frontend is not consent"
    );
    // Opening the old destination grants a real destination subscription. Age
    // it after resume so freshness cannot accidentally satisfy the predicate.
    send_request(
        &mut replacement,
        "thread/resume",
        /*id*/ 3,
        Some(json!({"threadId": destination, "excludeTurns": true})),
    )
    .await?;
    read_response_for_id(&mut replacement, /*id*/ 3)
        .await
        .context("open persisted destination before granting consent")?;
    let aged = age_destination(home.path(), &destination).await?;
    send_request(
        &mut replacement,
        "thread/read",
        /*id*/ 4,
        Some(json!({"threadId": destination, "includeTurns": false})),
    )
    .await?;
    let destination_view =
        to_response::<ThreadReadResponse>(read_response_for_id(&mut replacement, /*id*/ 4).await?)?;
    assert_eq!(destination_view.thread.updated_at, aged);
    let question = approval.params.as_ref().context("approval params")?["questions"][0]["id"]
        .as_str()
        .context("approval question id")?;
    send_jsonrpc(
        &mut replacement,
        JSONRPCMessage::Response(JSONRPCResponse {
            id: approval.id.clone(),
            result: json!({"answers": {question: {"answers": ["Allow"]}}}),
        }),
    )
    .await?;
    timeout(TIMEOUT, async {
        loop {
            if gate.calls.lock().await.len() >= 3 {
                break;
            }
            gate.changed.notified().await;
        }
    })
    .await
    .context("approved headless message was not delivered")?;
    let calls = gate.calls.lock().await.clone();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["input"].as_array().is_some_and(|input| {
                input.iter().any(|item| {
                    item["type"] == "function_call_output"
                        && item["output"]
                            .as_str()
                            .and_then(DelegatedMessage::parse)
                            .is_some_and(|message| {
                                message.source_thread_id == caller
                                    && message.input == "Only after consent"
                            })
                })
            }))
            .count(),
        1,
        "approval replay must not replay execution"
    );
    process.kill().await?;
    model_server.abort();
    Ok(())
}
