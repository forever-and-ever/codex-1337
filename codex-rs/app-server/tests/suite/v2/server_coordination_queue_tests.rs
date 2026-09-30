//! Durable delivery and cancellation stay destination-owned when the frontend disappears.

use super::*;

#[tokio::test]
async fn queued_headless_messages_preserve_provenance_and_can_be_cancelled_before_delivery()
-> Result<()> {
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
    send_initialize_request(&mut frontend, /*id*/ 1, "queue-origin").await?;
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
    read_response_for_id(&mut frontend, /*id*/ 4).await?;
    timeout(TIMEOUT, gate.blocked_entered.notified()).await?;
    *gate.tools.lock().await = ["Deliver this", "Cancel this"]
        .into_iter()
        .enumerate()
        .map(|(index, prompt)| {
            responses::ev_function_call_with_namespace(
                &format!("queue-{index}"),
                "mcp__codex_tui",
                "send_message_to_thread",
                &json!({"threadId": destination, "prompt": prompt, "delivery": "queued"})
                    .to_string(),
            )
        })
        .collect();
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
    .context("headless caller did not queue both messages")?;

    let mut replacement = connect_websocket(address).await?;
    send_initialize_request(&mut replacement, /*id*/ 1, "queue-replacement").await?;
    read_response_for_id(&mut replacement, /*id*/ 1).await?;
    send_request(
        &mut replacement,
        "thread/queue/list",
        /*id*/ 2,
        Some(json!({"threadId": destination})),
    )
    .await?;
    let queued = read_response_for_id(&mut replacement, /*id*/ 2)
        .await?
        .result;
    let data = queued["data"].as_array().context("queued submissions")?;
    assert_eq!(data.len(), 2);
    let envelopes = data
        .iter()
        .map(|item| {
            DelegatedMessage::parse(
                item["toolOutput"]["output"]
                    .as_str()
                    .context("queue envelope")?,
            )
            .context("queue provenance")
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        envelopes
            .iter()
            .map(|message| (message.source_thread_id.as_str(), message.input.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (caller.as_str(), "Deliver this"),
            (caller.as_str(), "Cancel this")
        ]
    );
    assert!(envelopes.iter().all(|message| message.sent_at.is_some()));
    send_request(
        &mut replacement,
        "thread/queue/delete",
        /*id*/ 3,
        Some(json!({"threadId": destination, "queuedSubmissionId": data[1]["id"]})),
    )
    .await?;
    assert_eq!(
        read_response_for_id(&mut replacement, /*id*/ 3)
            .await?
            .result["deleted"],
        true
    );
    // Release the existing turn only after cancellation is durable. The queue
    // must dispatch the surviving envelope without requiring a destination subscription.
    gate.blocked_release.notify_one();
    timeout(TIMEOUT, gate.blocked_entered.notified()).await?;
    let calls = gate.calls.lock().await.clone();
    let delivery = calls.last().context("queued destination request")?;
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
    assert_eq!(messages, vec![envelopes[0].clone()]);
    gate.blocked_release.notify_one();
    send_request(
        &mut replacement,
        "thread/queue/list",
        /*id*/ 4,
        Some(json!({"threadId": destination})),
    )
    .await?;
    assert_eq!(
        read_response_for_id(&mut replacement, /*id*/ 4)
            .await?
            .result["data"],
        json!([])
    );
    process.kill().await?;
    model_server.abort();
    Ok(())
}
