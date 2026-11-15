//! Verifies that root service-tier changes reach existing child turns and future work.

use super::agent_control::HostRuntime;
use anyhow::Result;
use codex_core::AgentRuntimeOwnership;
use codex_core::TurnInputRequest;
use codex_core::config::AgentRoleConfig;
use codex_core::config::Config;
use codex_features::Feature;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::submit_thread_settings;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use test_case::test_case;

const ROOT_PROMPT: &str = "spawn the service-tier worker";
const CHILD_PROMPT: &str = "pause the service-tier worker";
const FOLLOWUP_PROMPT: &str = "continue the service-tier worker later";
const FRESH_ROOT_PROMPT: &str = "spawn another service-tier worker";
const FRESH_CHILD_PROMPT: &str = "finish the new service-tier worker";
const SPAWN_CALL_ID: &str = "spawn-service-tier-worker";
const FRESH_SPAWN_CALL_ID: &str = "spawn-fresh-service-tier-worker";
const PAUSE_CALL_ID: &str = "pause-service-tier-worker";
const PRIORITY_ROLE: &str = "priority-worker";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    let body = match request
        .headers
        .get("content-encoding")
        .and_then(|encoding| encoding.to_str().ok())
    {
        Some(encoding) if encoding.eq_ignore_ascii_case("zstd") => {
            zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()
        }
        _ => Some(request.body.clone()),
    };
    body.and_then(|body| String::from_utf8(body).ok())
        .is_some_and(|body| body.contains(text))
}

fn assert_request_service_tier(request: &ResponseMock, expected: Option<&str>) {
    let body = request.single_request().body_json();
    // The real endpoint rejects unknown functions in its reserved namespace before
    // inference. A permissive SSE fixture otherwise conceals that whole-turn failure.
    // Compaction requests have no tools.
    for namespace in body["tools"].as_array().into_iter().flatten() {
        assert_ne!(
            namespace["name"], "collaboration",
            "custom agent schemas must not enter the reserved namespace"
        );
    }
    assert_eq!(
        request
            .single_request()
            .body_json()
            .get("service_tier")
            .and_then(serde_json::Value::as_str),
        expected
    );
}

fn configure_priority_role(config: &mut Config) {
    for feature in [Feature::Collab, Feature::MultiAgentV2, Feature::UnifiedExec] {
        config
            .features
            .enable(feature)
            .expect("test config should allow feature update");
    }
    config.model_provider.request_max_retries = Some(0);
    config.model_provider.stream_max_retries = Some(0);
    let role_path = config.codex_home.join("priority-worker.toml");
    std::fs::write(&role_path, "service_tier = \"priority\"\n")
        .expect("priority role should be written");
    config.agent_roles.insert(
        PRIORITY_ROLE.to_string(),
        AgentRoleConfig {
            description: Some("Role with a configured priority tier".to_string()),
            config_file: Some(role_path.to_path_buf()),
            nickname_candidates: None,
        },
    );
}

async fn wait_for_turn_complete(thread: &codex_core::CodexThread) {
    wait_for_event(thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
}

async fn mount_root_collaboration_call(
    server: &wiremock::MockServer,
    prompt: &'static str,
    call_id: &'static str,
    tool: &'static str,
    arguments: serde_json::Value,
) {
    let response_id = format!("root-{call_id}");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            body_contains(request, prompt) && !body_contains(request, call_id)
        },
        sse(vec![
            ev_response_created(&response_id),
            if tool == "set_agent_service_tier" {
                ev_function_call(call_id, tool, &arguments.to_string())
            } else {
                ev_function_call_with_namespace(
                    call_id,
                    "wrangler_agents",
                    tool,
                    &arguments.to_string(),
                )
            },
            ev_completed(&response_id),
        ]),
    )
    .await;

    let completion_id = format!("root-{call_id}-finished");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            body_contains(request, prompt) && body_contains(request, call_id)
        },
        sse(vec![
            ev_response_created(&completion_id),
            ev_assistant_message(&format!("{completion_id}-message"), "worker started"),
            ev_completed(&completion_id),
        ]),
    )
    .await;
}

async fn mount_completed_child(
    server: &wiremock::MockServer,
    prompt: &'static str,
    root_prompt: &'static str,
) -> ResponseMock {
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            body_contains(request, prompt) && !body_contains(request, root_prompt)
        },
        sse(vec![
            ev_response_created(prompt),
            ev_assistant_message(&format!("{prompt}-message"), "worker completed"),
            ev_completed(prompt),
        ]),
    )
    .await
}

#[test_case(Some("priority"), None, None, None; "disabling fast mode updates inherited work")]
#[test_case(Some("priority"), Some("default"), None, None; "explicit root default updates inherited work")]
#[test_case(None, Some("priority"), None, None; "enabling fast mode updates inherited work")]
#[test_case(None, Some("priority"), Some("default"), None; "explicit child tier survives root changes")]
#[test_case(Some("priority"), Some("default"), None, Some("priority"); "parent boosts an active child")]
#[test_case(None, Some("priority"), Some("default"), Some("inherit"); "parent restores live inheritance")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_service_tier_change_updates_existing_subagent(
    initial_service_tier: Option<&str>,
    updated_service_tier: Option<&str>,
    child_tier: Option<&str>,
    live_tier: Option<&str>,
) -> Result<()> {
    let server = start_mock_server().await;
    let initial_service_tier_owned = initial_service_tier.map(str::to_string);
    let updated_request_service_tier = updated_service_tier
        .filter(|service_tier| *service_tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE);
    let child_request_tier = live_tier
        .map(|tier| {
            if tier == "inherit" {
                updated_service_tier
            } else {
                Some(tier)
            }
        })
        .unwrap_or(child_tier.or(updated_service_tier))
        .filter(|tier| *tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE);
    let mut builder = test_codex()
        .with_model("gpt-5.6-sol")
        .with_config(move |config| {
            config.service_tier = initial_service_tier_owned;
            configure_priority_role(config);
        });
    let test = builder.build_with_auto_env(&server).await?;
    assert!(!test.config.features.enabled(Feature::StepModelSwitching));
    let mut created_threads = test.thread_manager.subscribe_thread_created();
    // Keep the actual tool blocked independently of agent notifications while its parent changes its tier.
    let release = test.cwd.path().join("tier-release.fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&release)
            .status()?
            .success()
    );

    mount_root_collaboration_call(
        &server,
        ROOT_PROMPT,
        SPAWN_CALL_ID,
        "spawn_agent",
        json!({
            "message": CHILD_PROMPT,
            "task_name": "worker",
            "service_tier": child_tier.unwrap_or("inherit"),
            "agent_type": PRIORITY_ROLE,
            "fork_turns": "none",
        }),
    )
    .await;

    let initial_child_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, CHILD_PROMPT)
                && !body_contains(request, ROOT_PROMPT)
                && !body_contains(request, PAUSE_CALL_ID)
        },
        sse(vec![
            ev_response_created("child-paused"),
            ev_function_call(
                PAUSE_CALL_ID,
                "exec_command",
                &json!({ "cmd": "read -r < tier-release.fifo", "workdir": test.cwd.path(), "yield_time_ms": 30_000 }).to_string(),
            ),
            ev_completed("child-paused"),
        ]),
    )
    .await;
    let continued_child_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, PAUSE_CALL_ID) && !body_contains(request, ROOT_PROMPT)
        },
        sse(vec![
            ev_response_created("child-continued"),
            ev_assistant_message("child-continued-message", "child continued"),
            ev_completed("child-continued"),
        ]),
    )
    .await;
    test.submit_text_turn(ROOT_PROMPT).await?;
    let child_thread_id = created_threads.recv().await?;
    let child = test.thread_manager.get_thread(child_thread_id).await?;
    let original_child_service_tier = child.config_snapshot().await.service_tier;
    assert_eq!(original_child_service_tier.as_deref(), child_tier);
    wait_for_event_match(child.as_ref(), |event| match event {
        EventMsg::ExecCommandBegin(request) if request.call_id == PAUSE_CALL_ID => Some(()),
        EventMsg::Error(error) => panic!("child failed before pausing: {}", error.message),
        EventMsg::TurnComplete(completed) => {
            panic!("child completed before pausing: {completed:?}")
        }
        _ => None,
    })
    .await;
    assert_request_service_tier(
        &initial_child_request,
        child_tier
            .or(initial_service_tier)
            .filter(|tier| *tier != "default"),
    );

    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            service_tier: Some(updated_service_tier.map(str::to_string)),
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(
        child.config_snapshot().await.service_tier,
        original_child_service_tier,
        "the root routing policy must not rewrite child-owned settings"
    );

    if let Some(tier) = live_tier {
        mount_root_collaboration_call(
            &server,
            "retier the worker",
            "retier-worker",
            "set_agent_service_tier",
            json!({"target": "worker", "service_tier": tier}),
        )
        .await;
        test.submit_text_turn("retier the worker").await?;
        assert_eq!(
            child.config_snapshot().await.service_tier.as_deref(),
            (tier != "inherit").then_some(tier)
        );
    }

    tokio::fs::write(&release, b"continue\n").await?;
    wait_for_turn_complete(&child).await;
    assert_request_service_tier(&continued_child_request, child_request_tier);

    let child_compaction_request = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, "compaction_trigger"),
        sse(vec![
            ev_response_created("child-remote-compaction"),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": "compacted child history",
                },
            }),
            ev_completed("child-remote-compaction"),
        ]),
    )
    .await;
    child.submit(Op::Compact).await?;
    wait_for_turn_complete(&child).await;
    assert_request_service_tier(&child_compaction_request, child_request_tier);

    let future_child_request = mount_completed_child(&server, FOLLOWUP_PROMPT, ROOT_PROMPT).await;
    child
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: FOLLOWUP_PROMPT.to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_turn_complete(&child).await;
    assert_request_service_tier(&future_child_request, child_request_tier);

    mount_root_collaboration_call(
        &server,
        FRESH_ROOT_PROMPT,
        FRESH_SPAWN_CALL_ID,
        "spawn_agent",
        json!({
            "message": FRESH_CHILD_PROMPT,
            "task_name": "fresh",
            "agent_type": PRIORITY_ROLE,
            "fork_turns": "none",
        }),
    )
    .await;
    let fresh_child_request =
        mount_completed_child(&server, FRESH_CHILD_PROMPT, FRESH_ROOT_PROMPT).await;
    test.submit_text_turn(FRESH_ROOT_PROMPT).await?;
    let fresh_thread_id = created_threads.recv().await?;
    let fresh_thread = test.thread_manager.get_thread(fresh_thread_id).await?;
    wait_for_turn_complete(&fresh_thread).await;
    assert_request_service_tier(&fresh_child_request, updated_request_service_tier);
    Ok(())
}

#[test_case(AgentRuntimeOwnership::Controller, None, None; "controller inherited tier")]
#[test_case(AgentRuntimeOwnership::Controller, Some("priority"), Some("priority"); "controller explicit tier")]
#[test_case(AgentRuntimeOwnership::Host, None, Some("priority"); "host independent tier")]
#[test_case(AgentRuntimeOwnership::Host, Some("priority"), Some("priority"); "host explicit tier")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evicted_role_subagent_preserves_service_tier_selection(
    owner: AgentRuntimeOwnership,
    child_tier: Option<&str>,
    expected: Option<&str>,
) -> Result<()> {
    struct FinishingPause;
    struct FinishingBarrier {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    impl codex_extension_api::ThreadLifecycleContributor<Config> for FinishingBarrier {
        fn on_thread_start<'a>(
            &'a self,
            input: codex_extension_api::ThreadStartInput<'a, Config>,
        ) -> codex_extension_api::ExtensionFuture<'a, ()> {
            Box::pin(async move {
                if matches!(input.session_source,
                    codex_protocol::protocol::SessionSource::SubAgent(
                        codex_protocol::protocol::SubAgentSource::ThreadSpawn { agent_path: Some(path), .. }
                    ) if path.as_str() == "/root/replacement"
                ) {
                    input.thread_store.insert(FinishingPause);
                }
            })
        }
        fn on_thread_idle<'a>(
            &'a self,
            input: codex_extension_api::ThreadIdleInput<'a>,
        ) -> codex_extension_api::ExtensionFuture<'a, ()> {
            Box::pin(async move {
                if input.thread_store.get::<FinishingPause>().is_some() {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
            })
        }
    }
    let finishing = std::sync::Arc::new(FinishingBarrier {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let mut extensions = codex_extension_api::ExtensionRegistryBuilder::<Config>::new();
    extensions.thread_lifecycle_contributor(finishing.clone());
    let server = start_mock_server().await;
    if owner == AgentRuntimeOwnership::Host {
        extensions.thread_lifecycle_contributor(Arc::new(HostRuntime));
    }
    let mut builder = test_codex()
        .with_model("gpt-5.6-sol")
        .with_extensions(Arc::new(extensions.build()))
        .with_config(move |config| {
            config.service_tier = Some(match owner {
                AgentRuntimeOwnership::Controller => "priority".to_string(),
                AgentRuntimeOwnership::Host => SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string(),
            });
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
            configure_priority_role(config);
        });
    let test = builder.build_with_auto_env(&server).await?;
    let mut created_threads = test.thread_manager.subscribe_thread_created();

    mount_root_collaboration_call(
        &server,
        ROOT_PROMPT,
        SPAWN_CALL_ID,
        "spawn_agent",
        json!({
            "message": CHILD_PROMPT,
            "task_name": "original",
            "service_tier": if owner == AgentRuntimeOwnership::Controller {
                child_tier.or(Some("inherit"))
            } else {
                child_tier
            },
            "agent_type": PRIORITY_ROLE,
            "fork_turns": "none",
        }),
    )
    .await;
    let original_request = mount_completed_child(&server, CHILD_PROMPT, ROOT_PROMPT).await;
    test.submit_text_turn(ROOT_PROMPT).await?;
    let original_thread_id = created_threads.recv().await?;
    let original_thread = test.thread_manager.get_thread(original_thread_id).await?;
    wait_for_turn_complete(&original_thread).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        original_thread
            .runtime_holds()
            .subscribe()
            .wait_for(|activity| {
                matches!(
                    activity,
                    codex_core::runtime_holds::RuntimeActivity::Quiet { .. }
                )
            }),
    )
    .await??;
    assert_request_service_tier(&original_request, Some("priority"));
    drop(original_thread);

    mount_root_collaboration_call(
        &server,
        FRESH_ROOT_PROMPT,
        FRESH_SPAWN_CALL_ID,
        "spawn_agent",
        json!({
            "message": FRESH_CHILD_PROMPT,
            "task_name": "replacement",
            "fork_turns": "none",
        }),
    )
    .await;
    mount_completed_child(&server, FRESH_CHILD_PROMPT, FRESH_ROOT_PROMPT).await;
    test.submit_text_turn(FRESH_ROOT_PROMPT).await?;
    let replacement_thread_id = created_threads.recv().await?;
    let replacement_thread = test
        .thread_manager
        .get_thread(replacement_thread_id)
        .await?;
    wait_for_turn_complete(&replacement_thread).await;
    assert!(
        test.thread_manager
            .get_thread(original_thread_id)
            .await
            .is_err()
    );

    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            service_tier: Some(None),
            ..Default::default()
        },
    )
    .await?;
    // The terminal event is already visible, but this actual task is still in
    // its idle lifecycle callback. Capacity must fail before that callback can
    // finish, not queue behind its runtime hold.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        finishing.entered.notified(),
    )
    .await?;
    let blocked = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        test.thread_manager
            .ensure_multi_agent_v2_child_loaded(original_thread_id),
    )
    .await
    .expect("capacity must return while the finishing barrier is held");
    assert!(matches!(
        blocked.unwrap_err().details(),
        codex_protocol::error::CodexErrorDetails::AgentLimitReached { .. }
    ));
    assert!(
        test.thread_manager
            .get_thread(replacement_thread_id)
            .await
            .is_ok()
    );
    finishing.release.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        replacement_thread
            .runtime_holds()
            .subscribe()
            .wait_for(|activity| {
                matches!(
                    activity,
                    codex_core::runtime_holds::RuntimeActivity::Quiet { .. }
                )
            }),
    )
    .await??;
    test.thread_manager
        .ensure_multi_agent_v2_child_loaded(original_thread_id)
        .await?;
    let reloaded_thread = test.thread_manager.get_thread(original_thread_id).await?;
    let expected_configured_tier = match owner {
        AgentRuntimeOwnership::Controller => child_tier.map(str::to_string),
        AgentRuntimeOwnership::Host => expected.map(str::to_string),
    };
    assert_eq!(
        reloaded_thread
            .config_snapshot()
            .await
            .service_tier
            .as_deref(),
        expected_configured_tier.as_deref(),
        "reload preserves the child's selection rather than its parent's or role's"
    );

    let reloaded_request = mount_completed_child(&server, FOLLOWUP_PROMPT, ROOT_PROMPT).await;
    reloaded_thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: FOLLOWUP_PROMPT.to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_turn_complete(&reloaded_thread).await;
    assert_request_service_tier(&reloaded_request, expected);
    reloaded_thread.shutdown_and_wait().await?;
    test.codex.shutdown_and_wait().await?;

    Ok(())
}
