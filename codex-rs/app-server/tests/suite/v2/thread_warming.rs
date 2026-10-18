use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_sequence;
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
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tempfile::TempDir;
use tokio::time::timeout;

const READ_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 60);

// Cache evidence reaches attaching clients without conflating passive warmth and active warming.
// Other Responses providers do not inherit OpenAI's retention, and a restart invents no lease.
// Native Claude requests reach only api.anthropic.com, so its holds have no mock here.
#[tokio::test]
async fn observed_cache_usage_controls_warmth_across_completion_attach_and_restart() -> Result<()> {
    for provider in ["OpenAI", "Other Responses provider"] {
        let usage = [(0, 4096), (3072, 0), (0, 0), (3072, 0)];
        let replies = usage
            .iter()
            .enumerate()
            .map(|(index, (read, write))| {
                let id = format!("resp-{index}");
                let mut completed = responses::ev_completed(&id);
                completed["response"]["usage"] = json!({
                    "input_tokens": 4096,
                    "input_tokens_details": {
                        "cached_tokens": read,
                        "cache_write_tokens": write,
                    },
                    "output_tokens": 1,
                    "total_tokens": 4097,
                });
                responses::sse(vec![
                    responses::ev_response_created(&id),
                    responses::ev_assistant_message(&format!("msg-{index}"), "Done"),
                    completed,
                ])
            })
            .collect();
        let server = create_mock_responses_server_sequence(replies).await;
        let home = TempDir::new()?;
        MockResponsesConfig::new(&server.uri())
            .with_provider_name(provider)
            .with_model("gpt-6.1-sol")
            .write(home.path())?;
        let mut app = TestAppServer::builder()
            .with_codex_home(home.path())
            .build_initialized_with_timeout(READ_TIMEOUT)
            .await?;
        let request = app
            .send_thread_start_request_with_auto_env(ThreadStartParams::default())
            .await?;
        let started: ThreadStartResponse =
            timeout(READ_TIMEOUT, app.read_response(request)).await??;
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

        for (index, (read, write)) in usage.into_iter().enumerate() {
            let before = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
            let request = app
                .send_turn_start_request(TurnStartParams {
                    thread_id: thread_id.clone(),
                    input: vec![UserInput::Text {
                        text: format!("Turn {index}"),
                        text_elements: Vec::new(),
                    }],
                    ..Default::default()
                })
                .await?;
            let _: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
            let _ = timeout(
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
            let _: ThreadResumeResponse =
                timeout(READ_TIMEOUT, app.read_response(request)).await??;
            let warm = provider == "OpenAI" && read + write > 0;
            // The fixture retains earlier broadcast notifications while reading RPC replies.
            // Wait past those for this lease/state, rather than asserting on a stale broadcast.
            let attached = timeout(READ_TIMEOUT, async {
                loop {
                    let update: ThreadPromptCacheUpdatedNotification =
                        app.read_notification("thread/promptCache/updated").await?;
                    let current = match update.estimate {
                        PromptCacheEstimate::Warm { expires_at } => {
                            warm && expires_at >= before + 1800
                        }
                        PromptCacheEstimate::Cold => !warm,
                    };
                    if current {
                        break Ok::<_, anyhow::Error>(update);
                    }
                }
            })
            .await??;
            if warm {
                let PromptCacheEstimate::Warm { expires_at } = attached.estimate else {
                    panic!("OpenAI cache usage should establish warmth: {attached:?}");
                };
                let after = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
                assert!((before + 1800..=after + 1800).contains(&expires_at));
            } else {
                assert_eq!(attached.estimate, PromptCacheEstimate::Cold);
            }
            assert_eq!(attached.warming, PromptCacheWarming::Off);
        }

        let _ = timeout(READ_TIMEOUT, app.shutdown_gracefully()).await??;
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
        // Resume checkpoints settings, not cache evidence: no request lease survives in metadata.
        assert_eq!(restored.estimate, PromptCacheEstimate::Cold);
        assert_eq!(restored.warming, PromptCacheWarming::Off);
        let _ = timeout(READ_TIMEOUT, app.shutdown_gracefully()).await??;
    }
    Ok(())
}
