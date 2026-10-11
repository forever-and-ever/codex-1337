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
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::body_string_contains;
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
#[tokio::test]
async fn farm_spawns_and_replays_without_steering_parent(
    mode: ThreadFarmMode,
    parent_state: ParentState,
) -> Result<()> {
    const BACKGROUND: &str = "Unique parent background: the staging fleet is violet.";
    const TASK: &str = "Inspect only the delegated staging task.";
    let server = create_mock_responses_server_repeating_assistant("Done").await;
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
    MockResponsesConfig::new(&server.uri())
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
    child_model.slug = "farm-model".to_owned();
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
        // A slow parent inference must not become a spawn lock. The child uses its own model.
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_partial_json(json!({"model": "mock-model"})))
            .and(body_string_contains("Parent pending work"))
            .respond_with(
                ResponseTemplate::new(/*s*/ 200)
                    .set_delay(Duration::from_secs(/*secs*/ 30))
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(responses::sse(vec![responses::ev_completed(
                        "parent-pending",
                    )])),
            )
            .with_priority(1)
            .up_to_n_times(1)
            .mount(&server)
            .await;
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
    let parent_read: ThreadReadResponse = app
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
            parent_read.thread.status,
            ThreadStatus::Active { .. }
        )),
        ParentState::Idle | ParentState::Pristine => {
            assert_eq!(parent_read.thread.status, ThreadStatus::Idle)
        }
    }
    let requests = server
        .received_requests()
        .await
        .expect("captured model requests");
    let bodies = requests
        .iter()
        .filter(|request| request.url.path() == "/v1/responses")
        .map(wiremock::Request::body_json::<Value>)
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
        mode == ThreadFarmMode::FullHistory && !matches!(parent_state, ParentState::Pristine)
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
    assert_eq!(
        bodies
            .iter()
            .filter(|body| body["model"] == "mock-model")
            .count(),
        if matches!(parent_state, ParentState::Active) {
            2
        } else if matches!(parent_state, ParentState::Idle) {
            1
        } else {
            0
        },
        "child completion does not wake the parent"
    );
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
        let mut next = params.clone();
        next.request_id = uuid::Uuid::new_v4().to_string();
        let second: ThreadFarmResponse = app
            .request(|request_id| ClientRequest::ThreadFarm {
                request_id,
                params: next,
            })
            .await?;
        assert_eq!(second.agent_path, "/root/farm_impl_staging_1");
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
    Ok(())
}

#[tokio::test]
async fn farm_provider_lock_rejects_before_claiming_request() -> Result<()> {
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
    child_model.slug = "farm-model".to_owned();
    write_models_cache_with_models(home.path(), vec![parent_model]).await?;
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
    let params = ThreadFarmParams {
        thread_id: parent.id.clone(),
        request_id: uuid::Uuid::new_v4().to_string(),
        mode: ThreadFarmMode::FullHistory,
        task_name: None,
        input: vec![UserInput::Text {
            text: "Inspect staging".to_owned(),
            text_elements: Vec::new(),
        }],
    };
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
    let db =
        codex_state::StateRuntime::init(config.sqlite.clone(), config.model_provider_id.clone())
            .await?;
    assert_eq!(
        db.farm_request(ThreadId::from_string(&parent.id)?, &params.request_id)
            .await?,
        None
    );
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .iter()
            .all(|request| request.url.path() != "/v1/responses")
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
            codex_core::FarmMode::FullHistory,
            core_input
        ))?)
    );
    db.claim_farm_request(
        ThreadId::from_string(&parent.id)?,
        &params.request_id,
        &payload_sha256,
    )
    .await?;
    db.close().await;
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
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .iter()
            .all(|request| request.url.path() != "/v1/responses")
    );
    restarted.shutdown_gracefully().await?;
    Ok(())
}
