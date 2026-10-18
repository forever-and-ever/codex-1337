//! A stopped runtime is not an unavailable conversation. Exercise the actual
//! server service because the public API deliberately has no raw Core shutdown RPC.

use super::ConnectionSessionState;
use super::message_processor_tracing_tests::TEST_CONNECTION_ID;
use super::message_processor_tracing_tests::build_test_processor;
use super::message_processor_tracing_tests::read_response;
use crate::transport::AppServerTransport;
use anyhow::Context;
use anyhow::Result;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::InitializeResponse;
use codex_app_server_protocol::ThreadStartResponse;
use codex_core::config::ConfigBuilder;
use codex_login::AuthManager;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Op;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

#[tokio::test]
#[expect(
    clippy::used_underscore_binding,
    reason = "The test probes the upstream RAII field to verify headless coordination; production retains it for lifetime ownership"
)]
async fn stopped_runtime_is_restarted_headlessly() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let home = TempDir::new()?;
    write_mock_responses_config_toml(
        home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 8_192,
        Some(false),
        "mock_provider",
        "compact",
    )?;
    let config = Arc::new(
        ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?,
    );
    let auth =
        AuthManager::shared_from_config(config.as_ref(), /*enable_codex_api_key_env*/ false)
            .await?;
    let (processor, mut outgoing) = Box::pin(build_test_processor(config, auth)).await;
    let session = Arc::new(ConnectionSessionState::new(
        crate::transport::ConnectionOrigin::Stdio,
    ));
    for (index, request) in [
        json!({"id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "codex-app-server-tests", "version": "0.1.0"}
        }}),
        // This tracing harness has no state DB. Custom saved metadata is covered
        // by the public cold-destination fixture; this case owns runtime recovery.
        json!({"id": 2, "method": "thread/start", "params": {}}),
    ]
    .into_iter()
    .enumerate()
    {
        Box::pin(processor.process_request(
            TEST_CONNECTION_ID,
            serde_json::from_value(request)?,
            &AppServerTransport::Stdio,
            Arc::clone(&session),
        ))
        .await;
        if !session.initialized() {
            anyhow::bail!("test connection did not initialize");
        }
        if index == 0 {
            let _: InitializeResponse = read_response(&mut outgoing, /*request_id*/ 1).await;
            // The real transport registers the initialized connection. This
            // processor-only harness must do the same to observe turn events.
            processor
                .connection_initialized(TEST_CONNECTION_ID, /*request_attestation*/ false)
                .await;
        }
    }
    let started: ThreadStartResponse = read_response(&mut outgoing, /*request_id*/ 2).await;
    let service = &processor._thread_coordination;
    let (stopped, lease) = Box::pin(service.load(&started.thread.id))
        .await
        .map_err(|error| anyhow::anyhow!("{}", error.message))?;
    let original = stopped.config_snapshot().await;
    // Upstream starts speculative threads without a rollout. Complete a real
    // turn before shutdown so this fixture exercises durable stopped recovery.
    Box::pin(processor.process_request(
        TEST_CONNECTION_ID,
        serde_json::from_value(json!({"id": 3, "method": "turn/start", "params": {
            "threadId": started.thread.id, "input": [{"type": "text", "text": "Persist this destination"}]
        }}))?,
        &AppServerTransport::Stdio,
        Arc::clone(&session),
    )).await;
    timeout(Duration::from_secs(10), async {
        loop {
            let envelope = outgoing
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("outgoing channel closed"))?;
            let message = match envelope {
                crate::outgoing_message::OutgoingEnvelope::ToConnection { message, .. }
                | crate::outgoing_message::OutgoingEnvelope::Broadcast { message } => message,
            };
            match message {
                crate::outgoing_message::OutgoingMessage::AppServerNotification(envelope)
                    if matches!(
                        envelope.notification,
                        codex_app_server_protocol::ServerNotification::TurnCompleted(_)
                    ) =>
                {
                    break Ok::<_, anyhow::Error>(());
                }
                crate::outgoing_message::OutgoingMessage::Error(error) => {
                    anyhow::bail!("turn failed: {error:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .context("persisting destination: waiting for turn/completed")??;
    stopped.flush_rollout().await?;
    drop(lease);
    processor
        .connection_closed(TEST_CONNECTION_ID, session.as_ref())
        .await;
    let _ = stopped.submit(Op::Shutdown).await?;
    timeout(Duration::from_secs(10), async {
        while stopped.agent_status().await != AgentStatus::Shutdown {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("stopping destination: waiting for AgentStatus::Shutdown")?;
    let (restarted, lease) = Box::pin(service.load(&started.thread.id))
        .await
        .map_err(|error| anyhow::anyhow!("{}", error.message))?;
    assert!(
        !Arc::ptr_eq(&stopped, &restarted),
        "a shutdown runtime cannot receive a turn"
    );
    let restored = restarted.config_snapshot().await;
    assert_eq!(
        (&restored.model, &restored.model_provider_id, restored.cwd()),
        (&original.model, &original.model_provider_id, original.cwd()),
    );
    assert_ne!(restarted.agent_status().await, AgentStatus::Shutdown);
    drop(lease);
    processor.shutdown_threads().await;
    processor.clear_all_thread_listeners().await;
    processor.drain_background_tasks().await;
    Ok(())
}
