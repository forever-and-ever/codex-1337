use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::config::Config;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::McpServerContribution;
use codex_extension_api::McpServerContributionContext;
use codex_extension_api::McpServerContributor;
use codex_features::Feature;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_remote;
use core_test_support::stdio_server_bin;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

struct NativeNamespaceIsolation;

impl McpServerContributor<Config> for NativeNamespaceIsolation {
    fn id(&self) -> &'static str {
        "coordination_isolation_test"
    }

    fn contribute<'a>(
        &'a self,
        _context: McpServerContributionContext<'a, Config>,
    ) -> ExtensionFuture<'a, Vec<McpServerContribution>> {
        Box::pin(async {
            vec![McpServerContribution::ReserveToolNamespace {
                namespace: "mcp__codex_tui".into(),
            }]
        })
    }
}

// The old prepared call retains its MCP client after publication changes. Revoking only the
// catalog would let it dispatch on an orphaned browser socket or strand the in-flight request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_revocation_cancels_in_flight_call_without_stopping_thread() -> Result<()> {
    skip_if_remote!(Ok(()), "the stdio MCP executable is host-local");
    let responses_server = responses::start_mock_server().await;
    let model_response = responses::mount_sse_once(
        &responses_server,
        responses::sse(vec![
            responses::ev_response_created("resp-after-revoke"),
            responses::ev_assistant_message("msg-after-revoke", "still alive"),
            responses::ev_completed("resp-after-revoke"),
        ]),
    )
    .await;
    let fixture = test_codex().build_with_auto_env(&responses_server).await?;
    let scratch = TempDir::new()?;
    let enter_file = scratch.path().join("sync-entered");
    let config: codex_config::McpServerConfig = serde_json::from_value(json!({
        "command": stdio_server_bin()?,
        "required": true,
        "startup_timeout_sec": 10,
        "tool_timeout_sec": 60,
        "env": {"MCP_TEST_SYNC_ENTER_FILE": enter_file},
    }))?;
    let first = Arc::new(CancellationToken::new());
    Box::pin(fixture.codex.attach_mcp_extension(
        "leased_mcp".into(),
        config.clone(),
        "Use the leased tool.".into(),
        Arc::clone(&first),
    ))
    .await?;

    // An ordinary MCP refresh must not strand a still-valid lease.
    assert_eq!(
        fixture
            .codex
            .refresh_mcp_config(fixture.codex.config().await, fixture.config.clone())
            .await,
        codex_core::ConfigRefreshOutcome::Published
    );
    Box::pin(fixture.codex.call_mcp_tool(
        "leased_mcp",
        /*environment_id*/ None,
        "echo",
        Some(json!({"message": "after refresh"})),
        /*meta*/ None,
    ))
    .await?;

    let thread = Arc::clone(&fixture.codex);
    let in_flight = tokio::spawn(async move {
        Box::pin(thread.call_mcp_tool(
            "leased_mcp",
            /*environment_id*/ None,
            "sync",
            Some(json!({"sleep_after_ms": 30_000})),
            /*meta*/ None,
        ))
        .await
    });
    timeout(Duration::from_secs(10), async {
        while !enter_file.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("MCP call never reached the server")?;
    fixture.codex.detach_mcp_extension(&first).await;
    let Err(error) = timeout(Duration::from_secs(5), in_flight).await?? else {
        anyhow::bail!("revoked MCP call unexpectedly succeeded")
    };
    anyhow::ensure!(
        error.to_string().contains("extension MCP lease revoked"),
        "unexpected call error: {error}"
    );

    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "continue".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let _request = model_response.single_request();

    let replacement = Arc::new(CancellationToken::new());
    Box::pin(timeout(Duration::from_secs(5), async {
        loop {
            match Box::pin(fixture.codex.attach_mcp_extension(
                "leased_mcp".into(),
                config.clone(),
                "Use the leased tool.".into(),
                Arc::clone(&replacement),
            ))
            .await
            {
                Ok(()) => break Ok(()),
                Err(error) if error.to_string().contains("thread has an active turn") => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => break Err(error),
            }
        }
    }))
    .await??;
    fixture.codex.detach_mcp_extension(&first).await;
    Box::pin(fixture.codex.call_mcp_tool(
        "leased_mcp",
        /*environment_id*/ None,
        "echo",
        Some(json!({"message": "replacement"})),
        /*meta*/ None,
    ))
    .await?;
    fixture.codex.detach_mcp_extension(&replacement).await;
    Ok(())
}

// A daemon restart loses the in-memory lease but retains model history. The first resumed turn
// must explicitly retire the historical developer instruction instead of treating it as live.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_history_deactivates_unowned_extension_instructions() -> Result<()> {
    skip_if_remote!(Ok(()), "the stdio MCP executable is host-local");
    let server = responses::start_mock_server().await;
    let mut extensions = ExtensionRegistryBuilder::new();
    extensions.mcp_server_contributor(Arc::new(NativeNamespaceIsolation));
    let retained: codex_config::McpServerConfig = serde_json::from_value(json!({
        "command": stdio_server_bin()?, "required": true, "startup_timeout_sec": 10,
    }))?;
    let mut builder = test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_model("test-gpt-5.1-codex")
        .with_config(move |config| {
            config
                .features
                .enable(Feature::CodeModeOnly)
                .expect("Code Mode enabled");
            config
                .mcp_servers
                .set(std::collections::HashMap::from([(
                    "mcp__codex_tui".into(),
                    retained,
                )]))
                .expect("retained transport configured");
        });
    let initial = builder.build_with_auto_env(&server).await?;
    let cancellation = Arc::new(CancellationToken::new());
    let config = serde_json::from_value(json!({
        "command": stdio_server_bin()?,
        "required": true,
        "startup_timeout_sec": 10,
    }))?;
    Box::pin(initial.codex.attach_mcp_extension(
        "codex_tui".into(),
        config,
        "Distinctive connection-owned instruction.".into(),
        cancellation,
    ))
    .await?;
    let active_response = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_custom_tool_call(
                "lease-inventory",
                "exec",
                r#"text(ALL_TOOLS.filter(({name}) => name.startsWith("mcp__mcp__codex_tui_") && typeof tools[name] === "function").map(({name}) => name));"#,
            ),
            responses::ev_completed("active-turn"),
        ]),
    )
    .await;
    let inventory_response = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("inventory-turn")]),
    )
    .await;
    initial.submit_text_turn("Use this thread").await?;
    let active_request = active_response.single_request();
    assert!(active_request.body_contains_text("Distinctive connection-owned instruction."));
    assert!(active_request.body_contains_text("[connection-scoped MCP extension]"));
    // Isolation changes canonical names, not lease authority. A hardcoded
    // mcp__{raw} direct-only selector would constrain native coordination instead
    // and accidentally expose the leased transport through Code Mode.
    let body = active_request.body_json();
    let tools = body["tools"].as_array().context("model tools")?;
    let leased = tools
        .iter()
        .find(|tool| {
            tool["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("mcp__mcp__codex_tui_"))
        })
        .context("isolated lease must remain directly callable")?;
    let leased_namespace = leased["name"].as_str().context("hashed lease namespace")?;
    let echo = leased["tools"]
        .as_array()
        .context("leased tools")?
        .iter()
        .find(|tool| tool["name"] == "echo")
        .context("echo tool")?;
    assert_ne!(echo["defer_loading"], json!(true));
    // Deferred tools are deliberately omitted from exec's description. Inspect
    // its actual callable inventory rather than treating prompt text as routing.
    let inventory = super::code_mode::custom_tool_output_last_non_empty_text(
        &inventory_response.single_request(),
        "lease-inventory",
    )
    .context("Code Mode inventory")?;
    assert!(!inventory.contains(&format!("{leased_namespace}__echo")));
    assert!(
        inventory.contains("mcp__mcp__codex_tui_"),
        "ordinary retained tools must remain in Code Mode"
    );

    let resumed = builder.restart(&server, &initial).await?;
    let resumed_response = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("resumed-turn")]),
    )
    .await;
    resumed
        .submit_text_turn("Continue without the extension")
        .await?;
    assert!(
        resumed_response
            .single_request()
            .body_contains_text("Its previous developer instructions are inactive")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_attach_refuses_active_turn() -> Result<()> {
    skip_if_remote!(Ok(()), "the stdio MCP executable is host-local");
    let server = responses::start_mock_server().await;
    let fixture = test_codex().build_with_auto_env(&server).await?;
    let _pending = responses::mount_response_once(
        &server,
        responses::sse_response(responses::sse(vec![responses::ev_completed("late")]))
            .set_delay(Duration::from_secs(30)),
    )
    .await;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Keep this turn active".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnStarted(_))
    })
    .await;
    let config: codex_config::McpServerConfig =
        serde_json::from_value(json!({ "command": stdio_server_bin()? }))?;
    let error = Box::pin(fixture.codex.attach_mcp_extension(
        "leased_mcp".into(),
        config.clone(),
        "Temporary instructions".into(),
        Arc::new(CancellationToken::new()),
    ))
    .await
    .expect_err("active turn must reject attachment");
    anyhow::ensure!(
        error.to_string().contains("thread has an active turn"),
        "unexpected attach error: {error}"
    );
    fixture.codex.submit(Op::Interrupt).await?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    server.reset().await;
    let _completed = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("late")]),
    )
    .await;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Keep this turn active".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    Box::pin(fixture.codex.attach_mcp_extension(
        "leased_mcp".into(),
        config,
        "Temporary instructions".into(),
        Arc::new(CancellationToken::new()),
    ))
    .await?;
    Ok(())
}
