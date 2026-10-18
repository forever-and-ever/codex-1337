//! Direct-user child spawning while the parent remains active or idle, including durable replay.

use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::write_chatgpt_auth;
use app_test_support::write_models_cache_with_models;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadFarmMode;
use codex_app_server_protocol::ThreadFarmParams;
use codex_app_server_protocol::ThreadFarmResponse;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadRetireParams;
use codex_app_server_protocol::ThreadRetireResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use codex_login::AuthCredentialsStoreMode;
use codex_protocol::ThreadId;
use codex_protocol::protocol::MultiAgentVersion;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[derive(Clone, Copy)]
enum ParentState {
    Active,
    Idle,
    Pristine,
}

#[test_case::test_case(ThreadFarmMode::FullHistory, ParentState::Active; "full_active")]
#[test_case::test_case(ThreadFarmMode::FullHistory, ParentState::Idle; "full_idle")]
#[test_case::test_case(ThreadFarmMode::Fresh, ParentState::Active; "fresh_active")]
#[test_case::test_case(ThreadFarmMode::Fresh, ParentState::Idle; "fresh_idle")]
#[test_case::test_case(ThreadFarmMode::FullHistory, ParentState::Pristine; "full_pristine")]
#[test_case::test_case(ThreadFarmMode::Automatic, ParentState::Idle; "automatic_provider_default")]
#[tokio::test]
async fn farm_spawns_and_replays_without_steering_parent(
    mode: ThreadFarmMode,
    parent_state: ParentState,
) -> Result<()> {
    const BACKGROUND: &str = "Unique parent background: the staging fleet is violet.";
    const TASK: &str = "Inspect only the delegated staging task.";
    let response = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let (_parent_gate, parent_gate_rx) = oneshot::channel();
    let (child_gate, child_gate_rx) = oneshot::channel();
    let mut streams = Vec::new();
    if !matches!(parent_state, ParentState::Pristine) {
        streams.push(vec![StreamingSseChunk {
            gate: None,
            body: response.clone(),
        }]);
    }
    if matches!(parent_state, ParentState::Active) {
        streams.push(vec![StreamingSseChunk {
            gate: Some(parent_gate_rx),
            body: response.clone(),
        }]);
    }
    streams.push(vec![StreamingSseChunk {
        gate: Some(child_gate_rx),
        body: response.clone(),
    }]);
    // Ordinary terminal mail can wake the parent; subsequent explicit work and
    // the named-child replay case share the same finite mock response tail.
    streams.extend((0..16).map(|_| {
        vec![StreamingSseChunk {
            gate: None,
            body: response.clone(),
        }]
    }));
    let (server, _) = start_streaming_sse_server(streams).await;
    let home = TempDir::new()?;
    // A pristine scoped parent initializes its account binding on first materialization.
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("farm-access")
            .account_id("farm-account")
            .chatgpt_account_id("farm-account")
            .chatgpt_user_id("farm-user"),
        AuthCredentialsStoreMode::File,
    )?;
    let role = home.path().join("farm-role.toml");
    std::fs::write(
        &role,
        "model = \"role-model\"\nmodel_reasoning_effort = \"low\"\ndeveloper_instructions = \"Preserve the default role restriction.\"\n",
    )?;
    MockResponsesConfig::new(server.uri())
        .with_provider_config("requires_openai_auth = true")
        .with_root_config(&format!("chatgpt_base_url = {:?}", server.uri()))
        .enable_feature(Feature::MultiAgentV2)
        .with_extra_config(&format!("[agents.default]\ndescription = \"Restricted delegated role\"\nconfig_file = {:?}\n[wrangler.farm]\nmodel = \"farm-model\"\nreasoning_effort = \"high\"\ninstructions = \"Never modify production.\"", role.display().to_string()))
        .write(home.path())?;
    let config = core_test_support::load_default_config_for_test(&home).await;
    let mut parent_model =
        codex_core::test_support::construct_model_info_offline("mock-model", &config);
    parent_model.multi_agent_version = Some(MultiAgentVersion::V2);
    let mut child_model = parent_model.clone();
    "farm-model".clone_into(&mut child_model.slug);
    child_model.supported_reasoning_levels =
        vec![codex_protocol::openai_models::ReasoningEffortPreset {
            effort: codex_protocol::openai_models::ReasoningEffort::High,
            description: "Focused delegated reasoning".to_owned(),
        }];
    let catalog_path = home.path().join("models.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec(&json!({"models": [parent_model, child_model]}))?,
    )?;
    let config_path = home.path().join("config.toml");
    let mut config_document: toml_edit::DocumentMut =
        std::fs::read_to_string(&config_path)?.parse()?;
    config_document["model_catalog_json"] =
        toml_edit::value(catalog_path.to_string_lossy().into_owned());
    if mode == ThreadFarmMode::FullHistory && matches!(parent_state, ParentState::Idle) {
        config_document["wrangler"]["farm"]["reasoning_effort"] = toml_edit::value("low");
        config_document["wrangler"]["farm"]["providers"]["mock_provider"]["reasoning_effort"] =
            toml_edit::value("high");
    }
    if mode == ThreadFarmMode::Automatic {
        // The provider's model override wins while its absent effort inherits the
        // global executor effort, not the default role's low effort.
        config_document["wrangler"]["farm"]["model"] = toml_edit::value("unavailable-global-model");
        config_document["wrangler"]["farm"]["providers"]["mock_provider"]["model"] =
            toml_edit::value("farm-model");
    }
    std::fs::write(config_path, config_document.to_string())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized()
        .await?;
    let parent = app.start_thread(ThreadStartParams::default()).await?.thread;
    if !matches!(parent_state, ParentState::Pristine) {
        app.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: parent.id.clone(),
            input: vec![UserInput::Text {
                text: BACKGROUND.to_owned(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    }
    let active_turn = if matches!(parent_state, ParentState::Active) {
        // A gated parent inference must not become a spawn lock.
        let started: TurnStartResponse = app
            .request(|request_id| ClientRequest::TurnStart {
                request_id,
                params: TurnStartParams {
                    thread_id: parent.id.clone(),
                    input: vec![UserInput::Text {
                        text: "Parent pending work".to_owned(),
                        text_elements: Vec::new(),
                    }],
                    ..Default::default()
                },
            })
            .await?;
        Some(started.turn.id)
    } else {
        None
    };
    let initial_parent_requests = match parent_state {
        ParentState::Active => 2,
        ParentState::Idle => 1,
        ParentState::Pristine => 0,
    };
    timeout(
        Duration::from_secs(10),
        server.wait_for_request_count(initial_parent_requests),
    )
    .await?;
    let params = ThreadFarmParams {
        thread_id: parent.id.clone(),
        request_id: uuid::Uuid::new_v4().to_string(),
        mode,
        task_name: (mode == ThreadFarmMode::FullHistory
            && matches!(parent_state, ParentState::Idle))
        .then(|| "farm_impl_staging".to_owned()),
        input: vec![UserInput::Text {
            text: TASK.to_owned(),
            text_elements: Vec::new(),
        }],
    };
    let receipt: ThreadFarmResponse = timeout(
        Duration::from_secs(/*secs*/ 10),
        app.request(|request_id| ClientRequest::ThreadFarm {
            request_id,
            params: params.clone(),
        }),
    )
    .await??;
    let replay: ThreadFarmResponse = app
        .request(|request_id| ClientRequest::ThreadFarm {
            request_id,
            params: params.clone(),
        })
        .await?;
    assert_eq!(replay, receipt);
    if params.task_name.is_some() {
        assert_eq!(receipt.agent_path, "/root/farm_impl_staging");
    }
    timeout(
        Duration::from_secs(10),
        server.wait_for_request_count(initial_parent_requests + 1),
    )
    .await?;
    let parent_before_completion: ThreadReadResponse = app
        .request(|request_id| ClientRequest::ThreadRead {
            request_id,
            params: ThreadReadParams {
                thread_id: parent.id.clone(),
                include_turns: false,
            },
        })
        .await?;
    match parent_state {
        ParentState::Active => assert!(matches!(
            parent_before_completion.thread.status,
            ThreadStatus::Active { .. }
        )),
        ParentState::Idle | ParentState::Pristine => {
            assert_eq!(parent_before_completion.thread.status, ThreadStatus::Idle);
        }
    }
    assert_eq!(
        server.requests().await.len(),
        initial_parent_requests + 1,
        "farm submission itself cannot start a parent inference"
    );
    child_gate
        .send(())
        .expect("release child completion after independence check");
    let child: ThreadReadResponse = timeout(Duration::from_secs(/*secs*/ 10), async {
        loop {
            let read: ThreadReadResponse = app
                .request(|request_id| ClientRequest::ThreadRead {
                    request_id,
                    params: ThreadReadParams {
                        thread_id: receipt.child_thread_id.clone(),
                        include_turns: false,
                    },
                })
                .await?;
            if read.thread.status == ThreadStatus::Idle {
                break Ok::<_, anyhow::Error>(read);
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
        }
    })
    .await??;
    assert_eq!(child.thread.parent_thread_id, Some(parent.id.clone()));
    let requests = server.requests().await;
    let bodies = requests
        .iter()
        .map(|body| serde_json::from_slice::<Value>(body))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let children = bodies
        .iter()
        .filter(|body| body["model"] == "farm-model")
        .collect::<Vec<_>>();
    assert_eq!(
        children.len(),
        1,
        "a replay cannot create another child inference"
    );
    assert_eq!(
        children[0]["reasoning"]["effort"], "high",
        "the configured farm executor wins over role effort too"
    );
    let input = children[0]["input"].to_string();
    assert!(
        !input.contains("\"encrypted_content\""),
        "a human farm task must never become provider ciphertext",
    );
    assert_eq!(
        input.contains(BACKGROUND),
        mode != ThreadFarmMode::Fresh && !matches!(parent_state, ParentState::Pristine)
    );
    assert_eq!(
        input.matches(TASK).count(),
        1,
        "the assignment is genuine child input, not duplicated inherited steering"
    );
    assert!(input.contains("Never modify production."));
    if mode == ThreadFarmMode::Fresh {
        assert!(input.contains("Preserve the default role restriction."));
    }
    // Family mailbox delivery wakes idle recipients even for terminal result
    // messages carrying trigger_turn=false. Only genuine child mail may cause
    // extra parent inference; the gated assertion above isolates the submission.
    for body in bodies
        .iter()
        .filter(|body| body["model"] == "mock-model")
        .skip(initial_parent_requests)
    {
        let input = body["input"].to_string();
        assert!(
            input.contains("FINAL_ANSWER") && input.contains(&receipt.agent_path),
            "only child terminal mail may wake the parent: {input}"
        );
    }
    if let Some(turn_id) = active_turn {
        let _: TurnInterruptResponse = app
            .request(|request_id| ClientRequest::TurnInterrupt {
                request_id,
                params: TurnInterruptParams {
                    thread_id: parent.id.clone(),
                    turn_id,
                },
            })
            .await?;
    }
    if params.task_name.is_some() {
        // Invalid input is checked before protected identity access. It cannot
        // overwrite the accepted same-ID outcome with a safe-to-resubmit rejection.
        let mut invalid = params.clone();
        invalid.input.push(UserInput::Image {
            image: codex_app_server_protocol::ImageReference::Inline {
                url: "https://example.invalid/private.png".into(),
            },
            detail: None,
        });
        let id = app
            .send_request("thread/farm", Some(serde_json::to_value(&invalid)?))
            .await?;
        let error = app
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        assert_eq!(error.error.code, -32600);
        assert_eq!(
            error.error.data,
            Some(
                json!({"farmOutcome": "uncertain", "requestId": params.request_id, "childThreadId": null, "agentPath": null})
            )
        );
        let replay: ThreadFarmResponse = app
            .request(|request_id| ClientRequest::ThreadFarm {
                request_id,
                params: params.clone(),
            })
            .await?;
        assert_eq!(replay, receipt);

        let pool = config
            .sqlite
            .open_read_write_pool(&config.sqlite.state_db_path())
            .await?;
        // Enqueue both calls behind an owned SQLite admission barrier, not a sleep.
        let barrier = pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut next = params.clone();
        next.request_id = uuid::Uuid::new_v4().to_string();
        let first = app
            .send_request("thread/farm", Some(serde_json::to_value(&next)?))
            .await?;
        next.request_id = uuid::Uuid::new_v4().to_string();
        let second = app
            .send_request("thread/farm", Some(serde_json::to_value(&next)?))
            .await?;
        barrier.commit().await?;
        let first: ThreadFarmResponse = app.read_response(first).await?;
        let second: ThreadFarmResponse = app.read_response(second).await?;
        let mut paths = [first.agent_path, second.agent_path];
        paths.sort();
        assert_eq!(
            paths,
            ["/root/farm_impl_staging_1", "/root/farm_impl_staging_2"]
        );

        next.request_id = uuid::Uuid::new_v4().to_string();
        // Generated names must coalesce too, while the winner's path is only a placeholder.
        next.task_name = None;
        let barrier = pool.begin_with("BEGIN IMMEDIATE").await?;
        let first = app
            .send_request("thread/farm", Some(serde_json::to_value(&next)?))
            .await?;
        let second = app
            .send_request("thread/farm", Some(serde_json::to_value(&next)?))
            .await?;
        barrier.commit().await?;
        let ids = [RequestId::Integer(first), RequestId::Integer(second)];
        let mut accepted = None;
        let mut completed = 0;
        while completed < 2 {
            match app.read_next_message().await? {
                codex_app_server_protocol::JSONRPCMessage::Response(response)
                    if ids.contains(&response.id) =>
                {
                    let child: ThreadFarmResponse = serde_json::from_value(response.result)?;
                    if let Some(original) = &accepted {
                        assert_eq!(original, &child);
                    }
                    accepted = Some(child);
                    completed += 1;
                }
                codex_app_server_protocol::JSONRPCMessage::Error(error)
                    if ids.contains(&error.id) =>
                {
                    assert_eq!(
                        error.error.data.as_ref().map(|data| &data["farmOutcome"]),
                        Some(&json!("uncertain"))
                    );
                    completed += 1;
                }
                _ => {}
            }
        }
        let accepted = accepted.expect("one same-ID submission owns construction");
        let replay: ThreadFarmResponse = app
            .request(|request_id| ClientRequest::ThreadFarm {
                request_id,
                params: next.clone(),
            })
            .await?;
        assert_eq!(replay, accepted);

        // A real allocation precedes this failed pointer write. NULL is not proof
        // of nonallocation, and removing the fault cannot authorize another child.
        sqlx::query("CREATE TRIGGER farm_pointer_fault BEFORE UPDATE OF child_thread_id ON thread_farm_requests WHEN NEW.agent_path = '/root/farm_pointer_fault' BEGIN SELECT RAISE(ABORT, 'farm pointer fault'); END").execute(&pool).await?;
        next.request_id = uuid::Uuid::new_v4().to_string();
        next.task_name = Some("farm_pointer_fault".into());
        let fault = app
            .send_request("thread/farm", Some(serde_json::to_value(&next)?))
            .await?;
        let fault = app
            .read_stream_until_error_message(RequestId::Integer(fault))
            .await?;
        sqlx::query("DROP TRIGGER farm_pointer_fault")
            .execute(&pool)
            .await?;
        assert_eq!(
            fault.error.data,
            Some(
                json!({"farmOutcome": "uncertain", "requestId": next.request_id, "childThreadId": null, "agentPath": null})
            )
        );
        assert!(fault.error.message.contains("was allocated"));
        let replay = app
            .send_request("thread/farm", Some(serde_json::to_value(&next)?))
            .await?;
        let replay = app
            .read_stream_until_error_message(RequestId::Integer(replay))
            .await?;
        assert_eq!(replay.error.data, fault.error.data);
        next.request_id = uuid::Uuid::new_v4().to_string();
        let next: ThreadFarmResponse = app
            .request(|request_id| ClientRequest::ThreadFarm {
                request_id,
                params: next,
            })
            .await?;
        assert_eq!(next.agent_path, "/root/farm_pointer_fault_1");
        pool.close().await;
    }
    // A farm must leave the loaded parent able to accept work and resume.
    timeout(
        Duration::from_secs(/*secs*/ 10),
        app.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: parent.id.clone(),
            input: vec![UserInput::Text {
                text: "Continue the parent after farming".to_owned(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        }),
    )
    .await??;
    let _: ThreadResumeResponse = timeout(
        Duration::from_secs(/*secs*/ 10),
        app.request(|request_id| ClientRequest::ThreadResume {
            request_id,
            params: ThreadResumeParams {
                thread_id: parent.id.clone(),
                exclude_turns: true,
                ..Default::default()
            },
        }),
    )
    .await??;
    let retired: ThreadRetireResponse = app
        .request(|request_id| ClientRequest::ThreadRetire {
            request_id,
            params: ThreadRetireParams {
                root_thread_id: parent.id.clone(),
                thread_id: receipt.child_thread_id.clone(),
            },
        })
        .await?;
    assert_eq!(retired.thread_ids, vec![receipt.child_thread_id.clone()]);
    app.shutdown_gracefully().await?;
    app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized()
        .await?;
    let _: ThreadResumeResponse = app
        .request(|request_id| ClientRequest::ThreadResume {
            request_id,
            params: ThreadResumeParams {
                thread_id: parent.id.clone(),
                exclude_turns: true,
                ..Default::default()
            },
        })
        .await?;
    let replay: ThreadFarmResponse = app
        .request(|request_id| ClientRequest::ThreadFarm {
            request_id,
            params: params.clone(),
        })
        .await?;
    assert_eq!(
        replay, receipt,
        "retirement/restart cannot erase an accepted receipt"
    );
    let mut mismatch = params;
    if mismatch.task_name.is_some() {
        mismatch.task_name = Some("farm_impl_other".into());
    } else {
        mismatch.input = vec![UserInput::Text {
            text: "A different task".to_owned(),
            text_elements: Vec::new(),
        }];
    }
    let id = app
        .send_request("thread/farm", Some(serde_json::to_value(&mismatch)?))
        .await?;
    let error = app
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert_eq!(
        error.error.data,
        Some(
            json!({"farmOutcome": "identityMismatch", "requestId": mismatch.request_id, "childThreadId": receipt.child_thread_id, "agentPath": receipt.agent_path})
        )
    );
    app.shutdown_gracefully().await?;
    server.shutdown().await;
    Ok(())
}

#[test_case::test_case(ThreadFarmMode::FullHistory; "explicit_history_rejects")]
#[test_case::test_case(ThreadFarmMode::Automatic; "automatic_global_fallback_is_fresh")]
#[tokio::test]
async fn farm_provider_lock_rejects_before_claiming_request(mode: ThreadFarmMode) -> Result<()> {
    const BACKGROUND: &str = "Private inherited parent context must not cross providers.";
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).enable_feature(Feature::MultiAgentV2)
        .enable_feature(Feature::ApiKeyModelDiscovery)
        .with_extra_config(&format!("[model_providers.other]\nname = \"Other provider\"\nbase_url = \"{}/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"test-bearer\"\nmodel_catalog_url = \"{}/other-models\"\n[wrangler.farm]\nmodel = \"other/farm-model\"", server.uri(), server.uri()))
        .write(home.path())?;
    let config = core_test_support::load_default_config_for_test(&home).await;
    let mut parent_model =
        codex_core::test_support::construct_model_info_offline("mock-model", &config);
    parent_model.multi_agent_version = Some(MultiAgentVersion::V2);
    let mut child_model = parent_model.clone();
    "farm-model".clone_into(&mut child_model.slug);
    Box::pin(write_models_cache_with_models(
        home.path(),
        vec![parent_model],
    ))
    .await?;
    Mock::given(method("GET"))
        .and(path("/other-models"))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({"models": [child_model]})),
        )
        .mount(&server)
        .await;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let parent = app.start_thread(ThreadStartParams::default()).await?.thread;
    app.start_turn_and_wait_for_completion(TurnStartParams {
        thread_id: parent.id.clone(),
        input: vec![UserInput::Text {
            text: BACKGROUND.to_owned(),
            text_elements: Vec::new(),
        }],
        ..Default::default()
    })
    .await?;
    let mut params = ThreadFarmParams {
        thread_id: parent.id.clone(),
        request_id: uuid::Uuid::new_v4().to_string(),
        mode,
        task_name: None,
        input: vec![UserInput::Text {
            text: "Inspect staging".to_owned(),
            text_elements: Vec::new(),
        }],
    };
    if mode == ThreadFarmMode::FullHistory {
        let id = app
            .send_request("thread/farm", Some(serde_json::to_value(&params)?))
            .await?;
        let error = app
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        assert!(
            error
                .error
                .message
                .contains("retain the parent model provider")
        );
        assert_eq!(
            error.error.data,
            Some(json!({"farmOutcome": "rejected", "requestId": params.request_id}))
        );
    } else {
        // A provider absent from the farming overrides still routes the global
        // executor safely, without inheriting provider-bound parent history.
        let receipt: ThreadFarmResponse = app
            .request(|request_id| ClientRequest::ThreadFarm {
                request_id,
                params: params.clone(),
            })
            .await?;
        let replay: ThreadFarmResponse = app
            .request(|request_id| ClientRequest::ThreadFarm {
                request_id,
                params: params.clone(),
            })
            .await?;
        assert_eq!(replay, receipt);
        timeout(Duration::from_secs(10), async {
            loop {
                let child: ThreadReadResponse = app
                    .request(|request_id| ClientRequest::ThreadRead {
                        request_id,
                        params: ThreadReadParams {
                            thread_id: receipt.child_thread_id.clone(),
                            include_turns: false,
                        },
                    })
                    .await?;
                if child.thread.status == ThreadStatus::Idle {
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        let requests = server.received_requests().await.expect("requests");
        let children = requests
            .iter()
            .filter(|request| request.url.path() == "/v1/responses")
            .map(wiremock::Request::body_json::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let children = children
            .iter()
            .filter(|body| body["model"] == "farm-model")
            .collect::<Vec<_>>();
        assert_eq!(children.len(), 1, "automatic replay cannot spawn twice");
        assert!(!children[0]["input"].to_string().contains(BACKGROUND));
        params.request_id = uuid::Uuid::new_v4().to_string();
    }
    let db =
        codex_state::StateRuntime::init(config.sqlite.clone(), config.model_provider_id.clone())
            .await?;
    let record = db
        .farm_request(ThreadId::from_string(&parent.id)?, &params.request_id)
        .await?;
    if mode == ThreadFarmMode::FullHistory {
        assert!(matches!(
            record.map(|record| record.state),
            Some(codex_state::FarmRequestState::Rejected(
                codex_state::FarmRejection {
                    kind: codex_state::FarmRejectionKind::InvalidRequest,
                    ..
                }
            ))
        ));
        let id = app
            .send_request("thread/farm", Some(serde_json::to_value(&params)?))
            .await?;
        let replay = app
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        assert!(
            replay
                .error
                .message
                .contains("retain the parent model provider")
        );
        assert_eq!(
            replay.error.data,
            Some(json!({"farmOutcome": "rejected", "requestId": params.request_id}))
        );
        let mut mismatch = params.clone();
        mismatch.input = vec![UserInput::Text {
            text: "Changed rejected submission".to_owned(),
            text_elements: Vec::new(),
        }];
        let id = app
            .send_request("thread/farm", Some(serde_json::to_value(&mismatch)?))
            .await?;
        let error = app
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        assert_eq!(
            error.error.data,
            Some(
                json!({"farmOutcome": "identityMismatch", "requestId": params.request_id, "childThreadId": null, "agentPath": null})
            )
        );
        // The durable rejection owns its ID, but no child path. Use a new
        // identity for the interrupted-reservation fixture below.
        params.request_id = uuid::Uuid::new_v4().to_string();
    } else {
        assert_eq!(record, None);
    }
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("requests")
            .iter()
            .filter(|request| request.url.path() == "/v1/responses")
            .filter(
                |request| request.body_json::<Value>().expect("response request")["model"]
                    == "farm-model"
            )
            .count(),
        usize::from(mode == ThreadFarmMode::Automatic)
    );
    // Simulate interruption immediately after durable claim. Reconciliation is not
    // permission to start a fresh child, even if current executor validation differs.
    let core_input = params
        .input
        .clone()
        .into_iter()
        .map(UserInput::into_core)
        .collect::<Vec<_>>();
    let payload_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            match mode {
                ThreadFarmMode::FullHistory => codex_core::FarmMode::FullHistory,
                ThreadFarmMode::Automatic => codex_core::FarmMode::Automatic,
                ThreadFarmMode::Fresh => unreachable!("test covers automatic and explicit history"),
            },
            core_input
        ))?)
    );
    db.claim_farm_request(
        ThreadId::from_string(&parent.id)?,
        &params.request_id,
        &payload_sha256,
        "/root/interrupted_farm",
    )
    .await?;
    db.close().await;
    // Replay must use the original requested mode before looking at today's
    // routing defaults; an uncertain claim is never permission to resend.
    let config_path = home.path().join("config.toml");
    let mut changed: toml_edit::DocumentMut = std::fs::read_to_string(&config_path)?.parse()?;
    changed["wrangler"]["farm"]["model"] = toml_edit::value("unavailable-after-claim");
    std::fs::write(config_path, changed.to_string())?;
    app.shutdown_gracefully().await?;
    let mut restarted = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let _: ThreadResumeResponse = restarted
        .request(|request_id| ClientRequest::ThreadResume {
            request_id,
            params: ThreadResumeParams {
                thread_id: parent.id,
                exclude_turns: true,
                ..Default::default()
            },
        })
        .await?;
    let id = restarted
        .send_request("thread/farm", Some(serde_json::to_value(&params)?))
        .await?;
    let error = restarted
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert_eq!(
        error.error.data,
        Some(
            json!({"farmOutcome": "uncertain", "requestId": params.request_id, "childThreadId": null, "agentPath": null})
        )
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("requests")
            .iter()
            .filter(|request| request.url.path() == "/v1/responses")
            .filter(
                |request| request.body_json::<Value>().expect("response request")["model"]
                    == "farm-model"
            )
            .count(),
        usize::from(mode == ThreadFarmMode::Automatic)
    );
    restarted.shutdown_gracefully().await?;
    Ok(())
}
