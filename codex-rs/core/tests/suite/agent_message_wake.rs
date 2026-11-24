//! Mail wakes keep exact admission; finishing turns retain status until publication.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_protocol::AgentPath;
use codex_protocol::config_types::ModeKind;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::ThreadIdle;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;
use tokio::sync::Notify;
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::wake_reservation::GatedModelsManager;

#[derive(Default)]
struct PauseOnce {
    gate: Mutex<Option<oneshot::Receiver<()>>>,
    entered: Notify,
}

impl PauseOnce {
    fn arm(&self) -> oneshot::Sender<()> {
        let (release, gate) = oneshot::channel();
        assert!(
            self.gate
                .lock()
                .expect("pause lock")
                .replace(gate)
                .is_none()
        );
        release
    }

    async fn pause(&self) {
        let gate = self.gate.lock().expect("pause lock").take();
        if let Some(gate) = gate {
            self.entered.notify_one();
            gate.await.expect("test should release the pause");
        }
    }
}

impl codex_extension_api::TurnLifecycleContributor for PauseOnce {
    fn on_turn_stop<'a>(
        &'a self,
        _input: codex_extension_api::TurnStopInput<'a>,
    ) -> codex_extension_api::ExtensionFuture<'a, ()> {
        Box::pin(self.pause())
    }
}

#[derive(Clone, Copy)]
enum WakeBoundary {
    Interrupt,
    Plan,
}

#[derive(Clone, Copy)]
enum AgentMessageWake {
    Enabled,
    Disabled,
}

#[test_case(WakeBoundary::Interrupt, AgentMessageWake::Enabled; "interrupt_during_discovery")]
#[test_case(WakeBoundary::Plan, AgentMessageWake::Enabled; "plan_mode_during_discovery")]
#[test_case(WakeBoundary::Interrupt, AgentMessageWake::Disabled; "disabled_interrupt_preserves_status_and_explicit_followup")]
#[test_case(WakeBoundary::Plan, AgentMessageWake::Disabled; "disabled_plan_allows_explicit_followup")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_agent_message_wake_respects_control_changes(
    boundary: WakeBoundary,
    setting: AgentMessageWake,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let wake_feature_enabled = matches!(setting, AgentMessageWake::Enabled);
    let server = responses::start_mock_server().await;
    let mut answer = responses::ev_assistant_message("answer", "Finished.");
    answer["item"]["phase"] = json!("final_answer");
    let response = responses::sse(vec![answer, responses::ev_completed("response")]);
    responses::mount_sse_sequence(&server, vec![response; 2]).await;
    let completion = Arc::new(PauseOnce::default());
    let release_completion = completion.arm();
    let models = Arc::new(GatedModelsManager::new()?);
    let mut extensions = ExtensionRegistryBuilder::new();
    extensions.turn_lifecycle_contributor(completion.clone());
    extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
    let test = test_codex()
        .with_models_manager(models.clone())
        .with_extensions(Arc::new(extensions.build()))
        .with_config(move |config| {
            for feature in [Feature::Collab, Feature::MultiAgentV2] {
                config.features.enable(feature).expect("enable agent mail");
            }
            if wake_feature_enabled {
                config.features.enable(Feature::WakeOnAgentMessages)
            } else {
                config.features.disable(Feature::WakeOnAgentMessages)
            }
            .expect("configure agent-message wakes");
        })
        .build_with_auto_env(&server)
        .await?;
    let input = |text: &str| {
        TurnInputRequest::user_input(vec![UserInput::Text {
            text: text.to_owned(),
            text_elements: Vec::new(),
        }])
    };
    let TurnInputSubmission::Started {
        turn_id: initial_turn_id,
        ..
    } = test
        .codex
        .start_or_steer_turn(input("Finish the initial work."))
        .await?
    else {
        anyhow::bail!("the initial input must start a turn");
    };
    timeout(Duration::from_secs(10), completion.entered.notified()).await?;

    let trigger_turn = matches!(setting, AgentMessageWake::Disabled);
    let mail = |text: &str| Op::InterAgentCommunication {
        communication: InterAgentCommunication::new(
            AgentPath::root().join("worker").expect("valid worker path"),
            AgentPath::root(),
            Vec::new(),
            text.to_owned(),
            trigger_turn,
        ),
        start_options: Default::default(),
    };
    let first_mail = "First deferred worker update.";
    let second_mail = "Worker update after stop.";
    test.codex.submit(mail(first_mail)).await?;
    // The settings reply fences the preceding mailbox operation while the old
    // turn still owns admission, so only its completion can start the wake.
    test.codex
        .update_thread_settings(ThreadSettingsOverrides::default())
        .await?;
    let discovery = models.gate_next_refresh();
    release_completion.send(()).expect("completion is paused");
    timeout(Duration::from_secs(10), discovery.entered.notified()).await?;
    // Discovery is not a publication fence for the previous turn's status.
    wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::TurnComplete(done) if done.turn_id == initial_turn_id),
    )
    .await;
    // Queue-only mail allows an idle notification before discovery. Explicit
    // follow-ups suppress it until they have been delivered.
    if wake_feature_enabled {
        ThreadIdle::wait(&test.codex).await;
    }

    let expected_mode = match boundary {
        WakeBoundary::Interrupt => {
            let previous_status = test.codex.agent_status().await;
            test.codex.submit(Op::Interrupt).await?;
            test.codex
                .update_thread_settings(ThreadSettingsOverrides::default())
                .await?;
            let expected_status = if wake_feature_enabled {
                AgentStatus::Interrupted
            } else {
                previous_status
            };
            assert_eq!(test.codex.agent_status().await, expected_status);
            ModeKind::Default
        }
        WakeBoundary::Plan => {
            let mut collaboration_mode = test.codex.config_snapshot().await.collaboration_mode;
            collaboration_mode.mode = ModeKind::Plan;
            test.codex
                .update_thread_settings(ThreadSettingsOverrides {
                    collaboration_mode: Some(collaboration_mode),
                    ..Default::default()
                })
                .await?;
            assert_eq!(
                test.codex.config_snapshot().await.collaboration_mode.mode,
                ModeKind::Plan
            );
            ModeKind::Plan
        }
    };
    discovery.release.notify_one();
    let (expected_turn_id, expected_mail) = if matches!(boundary, WakeBoundary::Interrupt) {
        // Later family mail can wake an interrupted recipient. Only the replacement
        // may sample, and it must receive both the retained and the new messages.
        let replacement = test.codex.submit(mail(second_mail)).await?;
        (Some(replacement), vec![first_mail, second_mail])
    } else {
        (None, vec![first_mail])
    };

    let EventMsg::TurnStarted(started) = wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::TurnStarted(started) if started.turn_id != initial_turn_id),
    )
    .await
    else {
        unreachable!("waited for family mail to start a turn");
    };
    if let Some(expected_turn_id) = expected_turn_id {
        assert_eq!(started.turn_id, expected_turn_id);
    }
    assert_eq!(started.collaboration_mode_kind, expected_mode);
    wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::TurnComplete(done) if done.turn_id == started.turn_id),
    )
    .await;
    ThreadIdle::wait(&test.codex).await;
    test.codex.shutdown_and_wait().await?;

    let requests = responses::received_responses_requests(&server).await;
    assert_eq!(requests.len(), 2);
    assert!(requests[0].inputs_of_type("agent_message").is_empty());
    assert_eq!(
        requests[1].body_json()["client_metadata"]["turn_id"],
        started.turn_id
    );
    let messages = requests[1].inputs_of_type("agent_message");
    let actual_mail = messages
        .iter()
        .map(|message| {
            assert_eq!(message["author"], "/root/worker");
            assert_eq!(message["recipient"], "/root");
            message["content"].clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual_mail,
        expected_mail
            .iter()
            .map(|text| json!([{"type": "input_text", "text": text}]))
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test_case(AgentMessageWake::Enabled; "enabled")]
#[test_case(AgentMessageWake::Disabled; "disabled")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_during_completion_preserves_running_status(
    setting: AgentMessageWake,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("answer", "Finished."),
            responses::ev_completed("response"),
        ]),
    )
    .await;
    let barriers = Arc::new(PauseOnce::default());
    let release_completion = barriers.arm();
    let mut extensions = ExtensionRegistryBuilder::new();
    extensions.turn_lifecycle_contributor(barriers.clone());
    let test = test_codex()
        .with_model("gpt-5.4")
        .with_model_info_override("gpt-5.4", |model| {
            model.multi_agent_version = Some(MultiAgentVersion::V2);
        })
        .with_extensions(Arc::new(extensions.build()))
        .with_config(move |config| {
            config
                .features
                .disable(Feature::MultiAgentV2)
                .expect("select runtime from model metadata");
            match setting {
                AgentMessageWake::Enabled => config.features.enable(Feature::WakeOnAgentMessages),
                AgentMessageWake::Disabled => config.features.disable(Feature::WakeOnAgentMessages),
            }
            .expect("configure agent-message wakes");
        })
        .build_with_auto_env(&server)
        .await?;
    let turn_id = match setting {
        AgentMessageWake::Enabled => {
            assert_eq!(test.codex.multi_agent_version(), None);
            test.codex
                .submit(Op::InterAgentCommunication {
                    communication: InterAgentCommunication::new(
                        AgentPath::root().join("worker").expect("valid worker path"),
                        AgentPath::root(),
                        Vec::new(),
                        "Finish the work.".to_owned(),
                        /*trigger_turn*/ false,
                    ),
                    start_options: Default::default(),
                })
                .await?;
            let EventMsg::TurnStarted(started) = wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::TurnStarted(_))
            })
            .await
            else {
                unreachable!("waited for turn start")
            };
            started.turn_id
        }
        AgentMessageWake::Disabled => {
            let TurnInputSubmission::Started { turn_id, .. } = test
                .codex
                .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                    text: "Finish the work.".to_owned(),
                    text_elements: Vec::new(),
                }]))
                .await?
            else {
                anyhow::bail!("the initial input must start a turn");
            };
            turn_id
        }
    };
    // The stop callback runs after the task is detached, before TurnComplete
    // is persisted. This is a finishing turn, not an unstarted wake reservation.
    timeout(Duration::from_secs(10), barriers.entered.notified()).await?;
    assert_eq!(test.codex.agent_status().await, AgentStatus::Running);
    test.codex.submit(Op::Interrupt).await?;
    test.codex
        .update_thread_settings(ThreadSettingsOverrides::default())
        .await?;
    let status_while_finishing = test.codex.agent_status().await;

    release_completion.send(()).expect("completion is paused");
    let completed = wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::TurnComplete(done) if done.turn_id == turn_id),
    )
    .await;
    let rollout_path = test.codex.rollout_path().expect("rollout path");
    test.codex.shutdown_and_wait().await?;

    // Interrupted would make a child eligible for capacity eviction while its
    // completion callback still owns the rollout writer.
    assert_eq!(status_while_finishing, AgentStatus::Running);
    let terminal_events = codex_rollout::read_rollout_text(rollout_path)?
        .lines()
        .map(codex_rollout::parse_rollout_line)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|line| match line.item {
            RolloutItem::EventMsg(
                event @ (EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_)),
            ) => Some(event),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_value(terminal_events)?,
        serde_json::to_value(vec![completed])?
    );
    response.single_request();
    Ok(())
}
