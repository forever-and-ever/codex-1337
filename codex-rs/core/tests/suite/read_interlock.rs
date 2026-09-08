use anyhow::Result;
use codex_features::Feature;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use core_test_support::responses::*;
use core_test_support::test_codex::TestCodexHarness;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::json;

async fn read(
    harness: &TestCodexHarness,
    code_mode: bool,
    command: &str,
    budget: usize,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let args = json!({"cmd": command, "login": false, "max_output_tokens": budget, "yield_time_ms": 10000});
    let call = if code_mode {
        ev_custom_tool_call(
            &id,
            "exec",
            &format!("text(await tools.exec_command({args}));"),
        )
    } else {
        ev_function_call(&id, "exec_command", &args.to_string())
    };
    mount_sse_once(harness.server(), sse(vec![call, ev_completed(&id)])).await;
    let result = mount_sse_once(
        harness.server(),
        sse(vec![
            ev_assistant_message("done", "done"),
            ev_completed("done"),
        ]),
    )
    .await;
    harness
        .test()
        .submit_turn("Inspect the requested source.")
        .await?;
    let request = result.single_request();
    let item = if code_mode {
        request.custom_tool_call_output(&id)
    } else {
        request.function_call_output(&id)
    };
    match &item["output"] {
        serde_json::Value::String(text) => Ok(text.clone()),
        serde_json::Value::Array(items) => Ok(items
            .iter()
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")),
        _ => anyhow::bail!("missing read output: {item}"),
    }
}

#[test_case::test_case(false; "direct")]
#[test_case::test_case(true; "code_mode")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_interlock_shell_delivery_edits_compaction_and_force(code_mode: bool) -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let builder = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_config(move |config| {
            config
                .features
                .enable(Feature::UnifiedExec)
                .expect("enable unified exec");
            if code_mode {
                config
                    .features
                    .enable(Feature::CodeMode)
                    .expect("enable Code Mode");
            }
            // Exercise ordinary local compaction through the production task.
            config.model_provider.name = "read-test".into();
            config.model_provider.supports_websockets = false;
        });
    let harness = TestCodexHarness::with_auto_env_builder(builder).await?;
    let body = "source line with enough useful detail\n".repeat(40);
    harness.write_file("sample.rs", &body).await?;
    harness.write_file("truncated.rs", &body).await?;
    let first = read(&harness, code_mode, "cat sample.rs", 4000).await?;
    assert!(
        first.contains("Read R") && first.contains("whole file"),
        "{first}"
    );
    assert!(
        read(&harness, code_mode, "cat sample.rs", 4000)
            .await?
            .contains("Unchanged sample.rs")
    );
    assert!(
        !read(&harness, code_mode, "sed -n '1,20p' sample.rs", 4000)
            .await?
            .contains("Unchanged")
    );
    assert!(
        read(&harness, code_mode, "sed -n '1,20p' sample.rs", 4000)
            .await?
            .contains("Unchanged")
    );
    assert!(
        !read(&harness, code_mode, "# force_reread\ncat sample.rs", 4000)
            .await?
            .contains("Unchanged")
    );
    let short = read(&harness, code_mode, "cat truncated.rs", 35).await?;
    assert!(short.contains("truncated"), "{short}");
    assert!(
        !read(&harness, code_mode, "cat truncated.rs", 4000)
            .await?
            .contains("Unchanged")
    );
    assert!(
        read(&harness, code_mode, "cat truncated.rs", 4000)
            .await?
            .contains("Unchanged")
    );
    harness
        .write_file("sample.rs", body.replace("source", "edited"))
        .await?;
    assert!(
        read(&harness, code_mode, "cat sample.rs", 4000)
            .await?
            .contains("edited line")
    );

    mount_sse_once(
        harness.server(),
        sse(vec![
            ev_assistant_message("summary", "The source was inspected."),
            ev_completed("compact"),
        ]),
    )
    .await;
    harness.test().codex.submit(Op::Compact).await?;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert!(
        !read(&harness, code_mode, "cat sample.rs", 4000)
            .await?
            .contains("Unchanged")
    );

    let audit =
        tokio::fs::read_to_string(harness.test().config.log_dir.join("read-interlock").join(
            format!("{}.jsonl", harness.test().session_configured.session_id),
        ))
        .await?;
    assert!(audit.contains("suppressed") && audit.contains("changed") && audit.contains("forced"));
    assert!(!audit.contains("edited line") && !audit.contains("source line"));
    Ok(())
}
