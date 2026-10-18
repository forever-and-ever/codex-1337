use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn closed_idle_destination_bounces_messages_until_reopened() -> Result<()> {
    let gate = Arc::new(ModelGate::default());
    gate.first.store(/*val*/ true, Ordering::SeqCst);
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
    send_initialize_request(&mut frontend, /*id*/ 1, "comms-gate").await?;
    let _ = read_response_for_id(&mut frontend, /*id*/ 1).await?;
    let caller = start_thread(&mut frontend, /*id*/ 2).await?;
    let destination = start_thread(&mut frontend, /*id*/ 3).await?;
    persist_destination(&mut frontend, /*id*/ 4, &destination).await?;

    for (id, comms) in [(10, "closed"), (12, "open")] {
        send_request(
            &mut frontend,
            "thread/metadata/update",
            id,
            Some(json!({"threadId": destination, "comms": comms})),
        )
        .await?;
        let updated = read_response_for_id(&mut frontend, id).await?;
        assert_eq!(updated.result["thread"]["comms"], comms);
        gate.calls.lock().await.clear();
        let call_id = format!("comms-{comms}");
        *gate.tools.lock().await = vec![responses::ev_function_call_with_namespace(
            &call_id,
            "mcp__codex_tui",
            "send_message_to_thread",
            &json!({"threadId": destination, "prompt": "Continue the destination"}).to_string(),
        )];
        gate.first.store(/*val*/ false, Ordering::SeqCst);
        gate.release.notify_one();
        persist_destination(&mut frontend, id + 1, &caller).await?;
        let calls = gate.calls.lock().await.clone();
        if comms == "closed" {
            let output = calls
                .iter()
                .flat_map(|body| body["input"].as_array().into_iter().flatten())
                .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
                .context("message rejection")?["output"]
                .as_str()
                .context("rejection text")?;
            assert!(output.contains("Message undeliverable: that context is currently closed to work and is inactive."), "{output}");
            assert!(
                !calls
                    .iter()
                    .any(
                        |body| body["input"].as_array().into_iter().flatten().any(|item| {
                            item["output"]
                                .as_str()
                                .and_then(DelegatedMessage::parse)
                                .is_some()
                        })
                    )
            );
        } else {
            timeout(TIMEOUT, async {
                loop {
                    let delivered = gate.calls.lock().await.iter().any(|body| {
                        body["input"].as_array().into_iter().flatten().any(|item| {
                            item["output"]
                                .as_str()
                                .and_then(DelegatedMessage::parse)
                                .is_some_and(|message| {
                                    message.source_thread_id == caller
                                        && message.input == "Continue the destination"
                                })
                        })
                    });
                    if delivered {
                        break;
                    }
                    gate.changed.notified().await;
                }
            })
            .await
            .context("reopened destination did not receive the message")?;
        }
    }
    process.kill().await?;
    model_server.abort();
    Ok(())
}
