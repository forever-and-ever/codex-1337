//! Ordinary family mail wakes idle recipients without another user submission.

use super::*;
use codex_core::config::Config;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_protocol::protocol::Op;
use core_test_support::ThreadIdle;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[test_case::test_case(false, false; "singleton")]
#[test_case::test_case(true, false; "nested_active_family")]
#[test_case::test_case(false, true; "completion_wakes_idle_parent")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_mail_wakes_without_user_input(nested: bool, completion: bool) -> Result<()> {
    const MAIL: &str = "mail after the parent finished";
    const LATER: &str = "a second message in the same batch";
    let server = start_mock_server().await;
    mount_root_collaboration_call(
        &server,
        FIRST_PROMPT,
        "first-call",
        "spawn_agent",
        json!({"message": FIRST_TASK, "task_name": "first", "fork_turns": "none"}),
    )
    .await;
    let first_response = if nested {
        ev_function_call_with_namespace(
            "nested-spawn",
            MULTI_AGENT_V2_NAMESPACE,
            "spawn_agent",
            &json!({"message": SECOND_TASK, "task_name": "nested", "fork_turns": "none"})
                .to_string(),
        )
    } else {
        ev_function_call_with_namespace(
            "first-wait",
            MULTI_AGENT_V2_NAMESPACE,
            "wait_agent",
            r#"{"timeout_ms":3600000}"#,
        )
    };
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, FIRST_TASK) && !has_function_call_output(request, "first-call")
        },
        sse(vec![
            ev_response_created("worker"),
            first_response,
            ev_completed("worker"),
        ]),
    )
    .await;
    if nested {
        for (task, call_id, wait_id) in [
            (FIRST_TASK, Some("nested-spawn"), "first-wait"),
            (SECOND_TASK, None, "nested-wait"),
        ] {
            mount_sse_once_match(
                &server,
                move |request: &wiremock::Request| {
                    body_contains(request, task)
                        && call_id
                            .map_or(!has_function_call_output(request, "nested-spawn"), |id| {
                                has_function_call_output(request, id)
                            })
                },
                sse(vec![
                    ev_response_created(wait_id),
                    ev_function_call_with_namespace(
                        wait_id,
                        MULTI_AGENT_V2_NAMESPACE,
                        "wait_agent",
                        r#"{"timeout_ms":3600000}"#,
                    ),
                    ev_completed(wait_id),
                ]),
            )
            .await;
        }
    }
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
    let test = test_codex()
        .with_model("gpt-5.6-sol")
        .with_extensions(Arc::new(extensions.build()))
        .with_config(|config| {
            config.features.enable(Feature::Collab).unwrap();
            config.features.enable(Feature::MultiAgentV2).unwrap();
            config.multi_agent_v2.max_concurrent_threads_per_session = 4;
            config.multi_agent_v2.max_wait_timeout_ms = 3_600_000;
            config.multi_agent_v2.wait_agent_batch_window_ms = std::num::NonZeroU64::new(300);
        })
        .build_with_auto_env(&server)
        .await?;
    let mut created = test.thread_manager.subscribe_thread_created();
    test.submit_turn(FIRST_PROMPT).await?;
    ThreadIdle::wait(&test.codex).await;
    let first = test
        .thread_manager
        .get_thread(created.recv().await?)
        .await?;
    wait_for_event(&first, |event| {
        matches!(event, EventMsg::CollabWaitingBegin(_))
    })
    .await;
    let grandchild = if nested {
        let child = test
            .thread_manager
            .get_thread(created.recv().await?)
            .await?;
        wait_for_event(&child, |event| {
            matches!(event, EventMsg::CollabWaitingBegin(_))
        })
        .await;
        Some(child)
    } else {
        None
    };
    let resumed = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, MAIL),
        sse(vec![
            ev_response_created("mail-wake"),
            ev_assistant_message("mail-answer", "Received the worker updates."),
            ev_completed("mail-wake"),
        ]),
    )
    .await;
    let send = |text: &'static str| {
        let author = if nested {
            "/root/first/nested"
        } else {
            "/root/first"
        };
        test.codex.submit(Op::InterAgentCommunication {
            communication: codex_protocol::protocol::InterAgentCommunication::new(
                codex_protocol::AgentPath::try_from(author).unwrap(),
                codex_protocol::AgentPath::root(),
                Vec::new(),
                text.to_owned(),
                false,
            ),
            start_options: Default::default(),
        })
    };
    if completion {
        mount_sse_once_match(
            &server,
            |request: &wiremock::Request| has_function_call_output(request, "first-wait"),
            sse(vec![
                ev_response_created("worker-finished"),
                ev_assistant_message("worker-finished", MAIL),
                ev_completed("worker-finished"),
            ]),
        )
        .await;
        first
            .start_or_steer_turn(codex_protocol::turn_input::TurnInputRequest::user_input(
                vec![codex_protocol::user_input::UserInput::Text {
                    text: "finish your work".to_owned(),
                    text_elements: Vec::new(),
                }],
            ))
            .await?;
        wait_for_event(&first, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    } else {
        send(MAIL).await?;
    }
    if nested {
        // The durable timing invariant is exercised with virtual time in InputQueue's test.
        // Here the real server seam must not require another parent user submission.
        submit_thread_settings(&test.codex, ThreadSettingsOverrides::default()).await?;
        assert!(resumed.requests().is_empty());
        send(LATER).await?;
    }
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    ThreadIdle::wait(&test.codex).await;
    let requests = resumed.requests();
    assert_eq!(requests.len(), 1);
    let messages = requests[0].inputs_of_type("agent_message");
    assert!(messages.iter().any(|item| item.to_string().contains(MAIL)));
    assert_eq!(
        messages.iter().any(|item| item.to_string().contains(LATER)),
        nested
    );
    assert!(
        !messages
            .iter()
            .any(|item| item.to_string().contains("NEW_TASK"))
    );
    drop(grandchild);
    test.thread_manager
        .shutdown_all_threads_bounded(Duration::from_secs(5))
        .await;
    Ok(())
}
