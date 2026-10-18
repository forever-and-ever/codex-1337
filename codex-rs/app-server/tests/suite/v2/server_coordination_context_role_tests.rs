use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn context_role_is_discoverable_until_cleared() -> Result<()> {
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
    let (mut process, address) = spawn_websocket_server(home.path()).await?;
    let mut frontend = connect_websocket(address).await?;
    send_initialize_request(&mut frontend, /*id*/ 1, "context-role").await?;
    read_response_for_id(&mut frontend, /*id*/ 1).await?;
    let caller = start_thread(&mut frontend, /*id*/ 2).await?;
    let destination = start_thread(&mut frontend, /*id*/ 3).await?;
    persist_destination(&mut frontend, /*id*/ 4, &destination).await?;

    for (id, role) in [(10, Some("  Review agent boundaries  ")), (12, None)] {
        send_request(
            &mut frontend,
            "thread/contextRole/set",
            id,
            Some(json!({"threadId": destination, "contextRole": role})),
        )
        .await?;
        let updated = read_response_for_id(&mut frontend, id).await?;
        assert_eq!(updated.result, json!({"contextRole": role.map(str::trim)}));
        // Rollout reconciliation must preserve explicit SQLite-only roles, including clears.
        persist_destination(&mut frontend, id + 100, &destination).await?;
        gate.calls.lock().await.clear();
        let call_id = format!("context-role-{id}");
        *gate.tools.lock().await = vec![responses::ev_function_call_with_namespace(
            &call_id,
            "mcp__codex_tui",
            "list_threads",
            "{}",
        )];
        gate.first.store(/*val*/ false, Ordering::SeqCst);
        gate.release.notify_one();
        persist_destination(&mut frontend, id + 1, &caller).await?;
        let calls = gate.calls.lock().await;
        let output = calls
            .iter()
            .flat_map(|body| body["input"].as_array().into_iter().flatten())
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
            .context("list_threads output")?["output"]
            .as_str()
            .context("list_threads text")?;
        let output: Value = serde_json::from_str(output)?;
        let thread = output["threads"]
            .as_array()
            .context("listed threads")?
            .iter()
            .find(|thread| thread["id"] == destination)
            .context("listed destination")?;
        assert_eq!(
            thread.get("contextRole"),
            role.map(|role| json!(role.trim())).as_ref()
        );
    }
    process.kill().await?;
    model_server.abort();
    Ok(())
}
