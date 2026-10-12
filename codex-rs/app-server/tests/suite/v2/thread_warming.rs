use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use codex_app_server_protocol::PromptCacheEstimate;
use codex_app_server_protocol::PromptCacheWarming;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadPromptCacheUpdatedNotification;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const READ_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 60);

// Completion usage and thread metadata do not establish a provider's retention policy.
// Attach/restart must report Cold (not confidently warm), and warming must refuse unsupported providers.
// Native Claude requests reach only api.anthropic.com, so its holds have no mock here.
#[tokio::test]
async fn unestablished_cache_retention_stays_cold_across_completion_attach_and_restart()
-> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_provider_name("OpenAI")
        .write(home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    let request = app
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let started: ThreadStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
    let thread_id = started.thread.id;

    let request = app
        .send_request(
            "thread/warming/set",
            Some(json!({"threadId": thread_id, "enabled": true})),
        )
        .await?;
    let refused = timeout(
        READ_TIMEOUT,
        app.read_stream_until_error_message(RequestId::Integer(request)),
    )
    .await??;
    assert_eq!(
        refused.error.message,
        "Only Claude conversations can be warmed."
    );

    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInput::Text {
                text: "Hello".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
    timeout(
        READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = app
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.clone(),
            ..Default::default()
        })
        .await?;
    let _: ThreadResumeResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
    let attached: ThreadPromptCacheUpdatedNotification = timeout(
        READ_TIMEOUT,
        app.read_notification("thread/promptCache/updated"),
    )
    .await??;
    // Actual usage does not establish a route/model retention guarantee.
    assert_eq!(attached.estimate, PromptCacheEstimate::Cold);
    assert_eq!(attached.warming, PromptCacheWarming::Off);

    timeout(READ_TIMEOUT, app.shutdown_gracefully()).await??;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    let request = app
        .send_thread_resume_request(ThreadResumeParams {
            thread_id,
            ..Default::default()
        })
        .await?;
    let _: ThreadResumeResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
    let restored: ThreadPromptCacheUpdatedNotification = timeout(
        READ_TIMEOUT,
        app.read_notification("thread/promptCache/updated"),
    )
    .await??;
    // Resume checkpoints settings, advancing updated_at without making a model request.
    // A restarted server has no observed request lease to recover from that metadata.
    assert_eq!(restored.estimate, PromptCacheEstimate::Cold);
    assert_eq!(restored.warming, PromptCacheWarming::Off);
    Ok(())
}
