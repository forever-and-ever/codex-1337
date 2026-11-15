//! Ordinary family mail wakes idle recipients without another user submission.

use super::*;
use crate::suite::settings_commits::COMMITTED_MODEL;
use crate::suite::settings_commits::PauseAfterCommit;
use codex_core::config::Config;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::MessageLifecycleContributor;
use codex_extension_api::MessageLifecycleInput;
use codex_protocol::ThreadId;
use codex_protocol::protocol::Op;
use core_test_support::ThreadIdle;
use pretty_assertions::assert_eq;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use tokio::sync::oneshot;

#[derive(Default)]
struct Messages(Mutex<HashSet<uuid::Uuid>>);
impl MessageLifecycleContributor for Messages {
    fn on_publish(&self, input: &MessageLifecycleInput) {
        let mut pending = self.0.lock().expect("messages lock");
        let token = uuid::Uuid::new_v4();
        input.data.insert(token);
        assert!(pending.insert(token));
    }
    fn on_consumed(&self, input: &MessageLifecycleInput, _recipient: ThreadId) {
        let mut pending = self.0.lock().expect("messages lock");
        let token = input.data.remove::<uuid::Uuid>().expect("published token");
        assert!(pending.remove(token.as_ref()));
    }
}

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
            config
                .features
                .enable(Feature::Collab)
                .expect("enable collaboration");
            config
                .features
                .enable(Feature::MultiAgentV2)
                .expect("enable V2 agents");
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
                codex_protocol::AgentPath::try_from(author).expect("fixture author path"),
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
    if completion {
        let path = test.codex.rollout_path().expect("saved parent history");
        let (history, _, _) = codex_rollout::RolloutRecorder::load_rollout_items(&path).await?;
        let id = history.iter().find_map(|item| match item {
            codex_rollout::RolloutItem::ResponseItem(item)
                if matches!(&item.item, codex_protocol::models::ResponseItem::AgentMessage { author, content, .. }
                    if author == "/root/first" && content.iter().any(|part| format!("{part:?}").contains(MAIL))) =>
            {
                item.id().cloned()
            }
            _ => None,
        }).expect("canonical accepted completion mail");
        assert!(id.as_str().starts_with("amsg_mail_"));
        let db = codex_state::StateRuntime::init(
            test.config.sqlite_config().clone(),
            test.config.model_provider_id.clone(),
        )
        .await?;
        let receipt = db
            .thread_queue()
            .agent_mail(test.session_configured.thread_id, &id)
            .await?
            .expect("completion origin tombstone");
        assert_eq!(receipt.state, codex_state::AgentMailState::Admitted);
        assert!(
            db.thread_queue()
                .pending_agent_mail(test.session_configured.thread_id)
                .await?
                .is_empty()
        );
        // Repeated recovery reconciles canonical identity without another model request.
        test.thread_manager
            .recover_accepted_agent_mail(
                test.session_configured.thread_id,
                test.session_configured.thread_id,
            )
            .await?;
        assert_eq!(resumed.requests().len(), 1);
    }
    drop(grandchild);
    test.thread_manager
        .shutdown_all_threads_bounded(Duration::from_secs(5))
        .await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_mail_preserves_publications_and_followup_order() -> Result<()> {
    const MAIL: &str = "Keep this note for later";
    const LATE_MAIL: &str = "Another note while dispatch is held";
    const FOLLOWUP: &str = "Read the pending note";
    const AFTER_FOLLOWUP: &str = "This note must follow the new task";
    let server = core_test_support::responses::start_mock_server().await;
    core_test_support::mock_server::run(&[&server], async {
        mount_root_collaboration_call(
            &server,
            FIRST_PROMPT,
            "first-call",
            "spawn_agent",
            json!({ "message": FIRST_TASK, "task_name": "first", "fork_turns": "none" }),
        )
        .await;
        mount_completed_worker(&server, FIRST_TASK, "first-call").await;
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut extensions = ExtensionRegistryBuilder::<Config>::new();
        extensions.config_contributor(Arc::new(PauseAfterCommit {
            gate: Mutex::new(Some((entered_tx, release_rx))),
        }));
        extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
        let messages = Arc::new(Messages::default());
        extensions.message_lifecycle_contributor(messages.clone());
        let test = test_codex()
            .with_model("gpt-5.6-sol")
            .with_extensions(Arc::new(extensions.build()))
            .with_config(|config| {
                config.features.enable(Feature::Collab).unwrap();
                config.features.enable(Feature::MultiAgentV2).unwrap();
                config.multi_agent_v2.max_concurrent_threads_per_session = 2;
                config.multi_agent_v2.hide_spawn_agent_metadata = true;
            })
            .build_with_auto_env(&server)
            .await?;
        let mut created = test.thread_manager.subscribe_thread_created();
        test.submit_turn(FIRST_PROMPT).await?;
        ThreadIdle::wait(&test.codex).await;
        let first_id = created.recv().await?;
        let first = test.thread_manager.get_thread(first_id).await?;
        wait_for_event(&first, |event| matches!(event, EventMsg::TurnComplete(_))).await;
        ThreadIdle::wait(&first).await;
        let resumed = Arc::clone(&first);
        resumed
            .submit(Op::ThreadSettings {
                thread_settings: ThreadSettingsOverrides {
                    model: Some(COMMITTED_MODEL.to_string()),
                    ..Default::default()
                },
                reply: None,
            })
            .await?;
        entered_rx.await?;

        mount_root_collaboration_call(
            &server,
            "send a note",
            "queued-mail",
            "send_message",
            json!({ "target": "first", "message": MAIL }),
        )
        .await;
        test.submit_turn("send a note").await?;
        ThreadIdle::wait(&test.codex).await;
        mount_root_collaboration_call(
            &server,
            "send while dispatch is held",
            "late-mail",
            "send_message",
            json!({ "target": "first", "message": LATE_MAIL }),
        )
        .await;
        test.submit_turn("send while dispatch is held").await?;
        ThreadIdle::wait(&test.codex).await;
        assert!(test.thread_manager.get_thread(first_id).await.is_ok());

        let retained = messages.0.lock().unwrap().clone();
        assert_eq!(retained.len(), 2);
        // Held dispatch preserves publication ownership until the first sample.
        mount_root_collaboration_call(
            &server,
            "drain the note",
            "followup",
            "followup_task",
            json!({ "target": "first", "message": FOLLOWUP }),
        )
        .await;
        let (at_sampling, pending_at_sampling) = mpsc::channel();
        mount_sse_once_match(
            &server,
            move |request: &wiremock::Request| {
                let matches = body_contains(request, FOLLOWUP)
                    && !has_function_call_output(request, "followup");
                if matches {
                    at_sampling
                        .send(messages.0.lock().unwrap().clone())
                        .unwrap();
                }
                matches
            },
            sse(vec![
                ev_response_created("worker-wait"),
                ev_function_call_with_namespace(
                    "wait-note",
                    MULTI_AGENT_V2_NAMESPACE,
                    "wait_agent",
                    r#"{"timeout_ms":1}"#,
                ),
                ev_completed("worker-wait"),
            ]),
        )
        .await;
        let delivered = mount_sse_once_match(
            &server,
            |request: &wiremock::Request| has_function_call_output(request, "wait-note"),
            sse(vec![
                ev_assistant_message("done", "Read all notes"),
                ev_completed("done"),
            ]),
        )
        .await;
        test.submit_turn("drain the note").await?;
        ThreadIdle::wait(&test.codex).await;
        mount_root_collaboration_call(
            &server,
            "send after followup",
            "after-followup",
            "send_message",
            json!({ "target": "first", "message": AFTER_FOLLOWUP }),
        )
        .await;
        test.submit_turn("send after followup").await?;
        ThreadIdle::wait(&test.codex).await;
        release_tx.send(())?;
        wait_for_event(&resumed, |event| matches!(event, EventMsg::TurnComplete(_))).await;
        ThreadIdle::wait(&resumed).await;
        assert!(
            pending_at_sampling.try_recv()?.is_disjoint(&retained),
            "both retained publications must be consumed before the first resumed sample"
        );
        let request = delivered
            .requests()
            .into_iter()
            .find(|request| request.function_call_output_text("wait-note").is_some())
            .expect("worker continuation after waiting for mail");
        let messages = request.inputs_of_type("agent_message");
        let expected = [MAIL, LATE_MAIL, FOLLOWUP, AFTER_FOLLOWUP];
        let received = messages
            .iter()
            .filter_map(|item| {
                expected
                    .iter()
                    .find(|text| item.to_string().contains(**text))
                    .copied()
            })
            .collect::<Vec<_>>();
        assert_eq!(received, expected);
        assert_eq!(test.thread_manager.list_thread_ids().await.len(), 2);

        test.thread_manager
            .shutdown_all_threads_bounded(Duration::from_secs(5))
            .await;
        Ok(())
    })
    .await
}
