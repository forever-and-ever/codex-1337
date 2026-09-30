//! Headless steering stays in the destination's active turn and account.

use super::*;
use codex_app_server_protocol::TurnStartResponse;

#[tokio::test]
async fn headless_message_steers_stale_active_destination_with_authentic_provenance() -> Result<()>
{
    let gate = Arc::new(ModelGate {
        blocked_model: Some("busy-destination".into()),
        ..Default::default()
    });
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
    create_config_toml(home.path(), &format!("http://{model_address}"), "never")?;
    let path = home.path().join("config.toml");
    let mut config = std::fs::read_to_string(&path)?;
    config.push_str(
        "\n[tools.thread_coordination.tools.send_message_to_thread]\napproval_mode = \"approve\"\n",
    );
    std::fs::write(path, config)?;
    let (mut process, address) = spawn_websocket_server(home.path()).await?;
    let mut frontend = connect_websocket(address).await?;
    send_initialize_request(&mut frontend, /*id*/ 1, "steering-origin").await?;
    read_response_for_id(&mut frontend, /*id*/ 1).await?;
    let caller = start_thread(&mut frontend, /*id*/ 2).await?;
    send_request(
        &mut frontend,
        "thread/start",
        /*id*/ 3,
        Some(json!({"model": "busy-destination"})),
    )
    .await?;
    let destination =
        to_response::<ThreadStartResponse>(read_response_for_id(&mut frontend, /*id*/ 3).await?)?
            .thread
            .id;
    send_request(
        &mut frontend,
        "turn/start",
        /*id*/ 4,
        Some(json!({"threadId": destination, "input": [{"type": "text", "text": "busy"}]})),
    )
    .await?;
    let original_turn =
        to_response::<TurnStartResponse>(read_response_for_id(&mut frontend, /*id*/ 4).await?)?
            .turn
            .id;
    timeout(TIMEOUT, gate.blocked_entered.notified()).await?;
    // Unattended active turns remain eligible even with stale persisted metadata.
    let aged = age_destination(home.path(), &destination).await?;
    send_request(
        &mut frontend,
        "thread/read",
        /*id*/ 6,
        Some(json!({"threadId": destination, "includeTurns": false})),
    )
    .await?;
    let destination_view =
        to_response::<ThreadReadResponse>(read_response_for_id(&mut frontend, /*id*/ 6).await?)?;
    assert_eq!(destination_view.thread.updated_at, aged);
    assert!(matches!(
        destination_view.thread.status,
        codex_app_server_protocol::ThreadStatus::Active { .. }
    ));
    *gate.tools.lock().await = vec![responses::ev_function_call_with_namespace(
        "steer-message",
        "mcp__codex_tui",
        "send_message_to_thread",
        &json!({"threadId": destination, "prompt": "Steer this"}).to_string(),
    )];
    send_request(
        &mut frontend,
        "turn/start",
        /*id*/ 5,
        Some(json!({"threadId": caller, "input": [{"type": "text", "text": "coordinate"}]})),
    )
    .await?;
    read_response_for_id(&mut frontend, /*id*/ 5).await?;
    timeout(TIMEOUT, gate.entered.notified()).await?;
    frontend.close(None).await?;
    drop(frontend);
    gate.release.notify_one();
    timeout(TIMEOUT, async {
        loop {
            if gate.calls.lock().await.len() >= 3 {
                break;
            }
            gate.changed.notified().await;
        }
    })
    .await
    .context("headless caller did not steer the destination")?;
    // The source accepted the message while the destination was still active.
    // Finishing this model response must continue the same turn, not start a new one.
    gate.blocked_release.notify_one();
    timeout(TIMEOUT, gate.blocked_entered.notified()).await?;
    let calls = gate.calls.lock().await.clone();
    let delivery = calls.last().context("steered destination request")?;
    assert_eq!(delivery["model"], "busy-destination");
    let messages = delivery["input"]
        .as_array()
        .context("destination history")?
        .iter()
        .filter_map(|item| {
            (item["name"] == "send_message_to_thread")
                .then(|| item["output"].as_str().and_then(DelegatedMessage::parse))
                .flatten()
        })
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].source_thread_id, caller);
    assert_eq!(messages[0].input, "Steer this");
    assert!(messages[0].sent_at.is_some());

    let mut replacement = connect_websocket(address).await?;
    send_initialize_request(&mut replacement, /*id*/ 1, "steering-replacement").await?;
    read_response_for_id(&mut replacement, /*id*/ 1).await?;
    send_request(
        &mut replacement,
        "thread/read",
        /*id*/ 2,
        Some(json!({"threadId": destination, "includeTurns": true})),
    )
    .await?;
    let view =
        to_response::<ThreadReadResponse>(read_response_for_id(&mut replacement, /*id*/ 2).await?)?;
    assert_eq!(
        view.thread
            .turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<Vec<_>>(),
        vec![original_turn.as_str()]
    );
    gate.blocked_release.notify_one();
    process.kill().await?;
    model_server.abort();
    Ok(())
}
