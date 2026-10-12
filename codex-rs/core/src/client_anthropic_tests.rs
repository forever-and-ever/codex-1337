use super::*;
use crate::agent::control::ProviderAccountSelection;
use crate::session::TurnInput;
use crate::session::tests::bind_model_client_for_tests;
use crate::session::tests::make_session_and_context_with_auth_config_home_and_rx;
use crate::tasks::RegularTask;
use codex_config::config_toml::ConfigToml;
use codex_features::Feature;
use codex_login::AuthHeaders;
use codex_login::anthropic::AccountIdentity;
use codex_login::anthropic::AccountInfo;
use codex_login::anthropic::AccountStore;
use codex_login::anthropic::SubscriptionGrant;
use codex_model_provider::AnthropicAuthContext;
use codex_model_provider::create_model_provider_with_anthropic_account;
use codex_model_provider_info::ModelProviderAdapter;
use codex_protocol::openai_models::AnthropicCacheTtl;
use codex_protocol::openai_models::AnthropicModelCapabilities;
use codex_protocol::openai_models::AnthropicThinkingMode;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use wiremock::Request as MockRequest;
use wiremock::matchers::header;

// The ordinary provider seam injects a hermetic transport. Native configuration
// itself still accepts only the official endpoint and carries no bearer override.
struct NativeBoundary {
    inner: SharedModelProvider,
    endpoint: String,
    store: AccountStore,
    account_a: AccountInfo,
    account_b: String,
    switch_once: AtomicBool,
    client: Mutex<Weak<ModelClient>>,
}

impl std::fmt::Debug for NativeBoundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBoundary").finish_non_exhaustive()
    }
}

impl ModelProvider for NativeBoundary {
    fn info(&self) -> &ModelProviderInfo {
        self.inner.info()
    }

    fn capabilities(&self) -> codex_model_provider::ProviderCapabilities {
        self.inner.capabilities()
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.inner.auth_manager()
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>> {
        self.inner.auth()
    }

    fn account_state(&self) -> ProviderAccountResult {
        self.inner.account_state()
    }

    fn anthropic_auth_context(&self) -> std::io::Result<Option<Arc<AnthropicAuthContext>>> {
        self.inner.anthropic_auth_context()
    }

    fn models_manager(
        &self,
        cache_path: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        self.inner.models_manager(cache_path, config_model_catalog)
    }

    fn api_provider(
        &self,
    ) -> ModelProviderFuture<'_, codex_protocol::error::Result<codex_api::Provider>> {
        Box::pin(async move {
            // capture_anthropic_auth has already run. Rotate A and commit B before
            // the first POST; its explicit 401 must recover the captured A only.
            if self.switch_once.swap(false, Ordering::SeqCst) {
                self.store
                    .upsert(
                        self.account_a.clone(),
                        SubscriptionGrant::bearer_only("rotated-a".into())?,
                    )
                    .await?;
                let client = self.client.lock().unwrap().upgrade().unwrap();
                client.commit_provider_account(ProviderAccountSelection::ClaudeSubscription {
                    id: self.account_b.clone(),
                });
            }
            let mut provider = self.inner.api_provider().await?;
            provider.base_url = self.endpoint.clone();
            Ok(provider)
        })
    }
}

#[tokio::test]
async fn native_request_pins_recovery_then_switches_with_exact_custom_tool_replay_and_cache_warm()
-> anyhow::Result<()> {
    let home = TempDir::new()?;
    let store = AccountStore::new(home.path().to_path_buf());
    let account_a = AccountInfo {
        identity: AccountIdentity::new("native-test-a")?,
        email: None,
        organization_uuid: None,
        organization_name: None,
    };
    let account_b = AccountInfo {
        identity: AccountIdentity::new("native-test-b")?,
        ..account_a.clone()
    };
    let id_a = store
        .upsert(
            account_a.clone(),
            SubscriptionGrant::bearer_only("original-a".into())?,
        )
        .await?;
    let id_b = store
        .upsert(
            account_b,
            SubscriptionGrant::bearer_only("bearer-b".into())?,
        )
        .await?;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer original-a"))
        .respond_with(ResponseTemplate::new(/*s*/ 401))
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let custom_input = "text('λ <&>');\n";
    let tool_frames = [
        json!({"type":"message_start","message":{"id":"msg-a","usage":{"input_tokens":3,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"retain prior thought","signature":"retained-signature/=="}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"exec-call","name":"functions__exec","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":json!({"input":custom_input}).to_string()}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}),
        json!({"type":"message_stop"}),
    ];
    let done_frames = [
        json!({"type":"message_start","message":{"id":"msg-b","usage":{"input_tokens":5,"output_tokens":0,"cache_read_input_tokens":100}}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":0}}),
        json!({"type":"message_stop"}),
    ];
    for (bearer, frames) in [
        ("Bearer rotated-a", tool_frames.as_slice()),
        ("Bearer bearer-b", done_frames.as_slice()),
    ] {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("authorization", bearer))
            .and(wiremock::matchers::body_partial_json(
                json!({"stream":true}),
            ))
            .respond_with(
                ResponseTemplate::new(/*s*/ 200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        frames
                            .iter()
                            .map(|frame| format!("data: {frame}\n\n"))
                            .collect::<String>(),
                    ),
            )
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
    }
    // Empty headers supply runtime policy/home only, not an OpenAI credential.
    let runtime = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::Headers(AuthHeaders::new(http::HeaderMap::new())),
        home.path().to_path_buf(),
    );
    let policy: ConfigToml = toml::from_str(
        r#"
        [[anthropic_prompt_cache.rules]]
        model = "claude-sonnet"
        efforts = ["medium"]
        ttl = "5m"
        "#,
    )?;
    let mut client = test_model_client(SessionSource::Cli)
        .with_anthropic_prompt_cache(policy.anthropic_prompt_cache);
    let mut info = client.state.provider.info().clone();
    info.adapter = ModelProviderAdapter::ClaudeSubscription;
    info.base_url = None;
    let boundary = Arc::new(NativeBoundary {
        inner: create_model_provider_with_anthropic_account(
            info,
            Some(Arc::clone(&runtime)),
            Some(id_a.to_string()),
        ),
        endpoint: format!("{}/v1", server.uri()),
        store,
        account_a,
        account_b: id_b.to_string(),
        switch_once: AtomicBool::new(true),
        client: Mutex::new(Weak::new()),
    });
    Arc::get_mut(&mut client.state).unwrap().provider = boundary.clone();
    let warmer = Arc::new(WarmCapture::default());
    client.bind_prompt_cache_observer(Some(warmer.clone()));
    let (events_tx, events_rx) = async_channel::unbounded();
    client.event_sender = Some(events_tx);
    let client = Arc::new(client);
    *boundary.client.lock().unwrap() = Arc::downgrade(&client);
    client.commit_provider_account(ProviderAccountSelection::ClaudeSubscription {
        id: id_a.to_string(),
    });
    let mut model = test_model_info();
    model.slug = "claude-sonnet-5-5".into();
    model
        .supported_reasoning_levels
        .push(ReasoningEffortPreset {
            effort: ReasoningEffort::High,
            description: "high".into(),
        });
    model.anthropic = Some(AnthropicModelCapabilities {
        thinking: AnthropicThinkingMode::Adaptive,
        max_output_tokens: std::num::NonZeroU32::new(/*n*/ 100).unwrap(),
        supported_efforts: vec![ReasoningEffort::Medium, ReasoningEffort::High],
        supports_structured_outputs: false,
    });
    let mut prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: "run".into() }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        tools: Arc::from([ToolSpec::Namespace(ResponsesApiNamespace {
            name: "functions".into(),
            description: "Code Mode".into(),
            tools: vec![ResponsesApiNamespaceTool::Custom(FreeformTool {
                name: "exec".into(),
                description: "Run code".into(),
                defer_loading: None,
                format: FreeformToolFormat {
                    r#type: "grammar".into(),
                    syntax: "lark".into(),
                    definition: "start: /[\\s\\S]+/".into(),
                },
            })],
        })]),
        ..Default::default()
    };
    let mut session = client.new_session();
    let metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        format!("{}:0", client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    let trace = InferenceTraceContext::disabled();
    let telemetry = test_session_telemetry();
    let mut stream = session
        .stream(
            &prompt,
            &model,
            &telemetry,
            /*effort*/ None,
            codex_protocol::config_types::ReasoningSummary::None,
            /*service_tier*/ None,
            &metadata,
            &trace,
        )
        .await?;
    let mut calls = Vec::new();
    while let Some(event) = stream.next().await {
        if let ResponseEvent::OutputItemDone(item) = event? {
            calls.push(item);
        }
    }
    let expected_call = ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: "exec-call".into(),
        name: "exec".into(),
        namespace: Some("functions".into()),
        input: custom_input.into(),
        internal_chat_message_metadata_passthrough: None,
    };
    for item in &mut calls {
        match item {
            ResponseItem::Reasoning { id, .. } | ResponseItem::CustomToolCall { id, .. } => {
                *id = None
            }
            _ => panic!("unexpected completed native item"),
        }
    }
    let expected_reasoning: ResponseItem = serde_json::from_value(json!({
        "type":"reasoning",
        "summary":[{"type":"summary_text","text":"retain prior thought"}],
        "encrypted_content":"retained-signature/=="
    }))?;
    assert_eq!(calls, vec![expected_reasoning, expected_call]);
    prompt.input.extend(calls);
    prompt.input.push(ResponseItem::from(
        ResponseInputItem::CustomToolCallOutput {
            call_id: "exec-call".into(),
            name: Some("exec".into()),
            output: FunctionCallOutputPayload::from_text("result λ".into()),
        },
    ));
    let mut stream = session
        .stream(
            &prompt,
            &model,
            &telemetry,
            Some(ReasoningEffort::High),
            codex_protocol::config_types::ReasoningSummary::None,
            /*service_tier*/ None,
            &metadata,
            &trace,
        )
        .await?;
    while let Some(event) = stream.next().await {
        event?;
    }
    let requests = server.received_requests().await.unwrap();
    let observed = requests.iter().map(|request| {
        let mut body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let markers = body["tools"].as_array().unwrap().iter()
            .chain(body["system"].as_array().unwrap())
            .chain(body["messages"].as_array().unwrap().iter()
                .flat_map(|message| message["content"].as_array().unwrap()))
            .filter_map(|block| block.get("cache_control"))
            .cloned()
            .collect::<Vec<_>>();
        for message in body["messages"].as_array_mut().unwrap() {
            for block in message["content"].as_array_mut().unwrap() {
                block.as_object_mut().unwrap().remove("cache_control");
            }
        }
        json!({"bearer":request.headers["authorization"].to_str().unwrap(), "messages":body["messages"], "effort":body["output_config"]["effort"], "markers":markers})
    }).collect::<Vec<_>>();
    let user = json!({"role":"user","content":[{"type":"text","text":"run"}]});
    let five_minutes = vec![json!({"type":"ephemeral", "ttl":"5m"}); 3];
    let one_hour = vec![json!({"type":"ephemeral", "ttl":"1h"}); 4];
    assert_eq!(
        observed,
        vec![
            json!({"bearer":"Bearer original-a","messages":[user], "effort":"medium", "markers":five_minutes}),
            json!({"bearer":"Bearer rotated-a","messages":[user], "effort":"medium", "markers":five_minutes}),
            json!({"bearer":"Bearer bearer-b", "effort":"high", "markers":one_hour, "messages":[user,
            {"role":"assistant","content":[
                {"type":"thinking","thinking":"retain prior thought","signature":"retained-signature/=="},
                {"type":"tool_use","id":"exec-call","name":"functions__exec","input":{"input":custom_input}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"exec-call","content":"result λ","is_error":false}]}]}),
        ]
    );
    // Each completed request reports the TTL its own model and effort resolved.
    assert_eq!(
        *warmer.lore.lock().unwrap(),
        [
            crate::WarmingLore::Claude {
                ttl: AnthropicCacheTtl::FiveMinutes
            },
            crate::WarmingLore::Claude {
                ttl: AnthropicCacheTtl::OneHour
            },
        ]
    );
    let replay = warmer
        .replay
        .lock()
        .unwrap()
        .take()
        .expect("cached inference has a warming request");
    let history_before = prompt.input.clone();
    while events_rx.try_recv().is_ok() {}
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer bearer-b"))
        .and(wiremock::matchers::body_partial_json(
            json!({"max_tokens":0,"stream":false}),
        ))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "usage":{"input_tokens":5,"output_tokens":0,"cache_read_input_tokens":100,
            "cache_creation_input_tokens":0}
        })))
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let (_, usage) = replay.send(|| true).await?.expect("wanted replay sent");
    assert_eq!(
        usage,
        codex_protocol::protocol::TokenUsage {
            input_tokens: 105,
            cached_input_tokens: 100,
            total_tokens: 105,
            ..Default::default()
        }
    );
    let requests = server.received_requests().await.unwrap();
    let mut expected: serde_json::Value = serde_json::from_slice(&requests[2].body)?;
    expected["max_tokens"] = json!(0);
    expected["stream"] = json!(false);
    let warm = requests.last().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&warm.body)?,
        expected
    );
    for name in ["anthropic-beta", "anthropic-version", "authorization"] {
        assert_eq!(warm.headers[name], requests[2].headers[name]);
    }
    assert_eq!(prompt.input, history_before);
    assert!(
        events_rx.try_recv().is_err(),
        "side replay emitted a protocol event"
    );
    server.verify().await;
    native_regular_replay_profiles(
        &server,
        &boundary,
        &model,
        &prompt.input,
        &runtime,
        home.path(),
    )
    .await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum NativeReplayProfile {
    Recover,
    Exhaust,
    Committed,
}

async fn native_regular_replay_profiles(
    server: &MockServer,
    boundary: &Arc<NativeBoundary>,
    model: &ModelInfo,
    history: &[ResponseItem],
    runtime: &Arc<AuthManager>,
    home: &Path,
) -> anyhow::Result<()> {
    // The stream, item lifecycle, discard, retry and prompt all belong to the real
    // RegularTask. No presentation or discard events are manufactured by this fixture.
    // Reasoning then text isolates custody; deltas never return to an older open
    // block, since interleaved delta attribution remains a separate limitation.
    let pending = |id: &str| {
        vec![
            json!({"type":"message_start","message":{"id":id,"usage":{"input_tokens":1,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"FAILED_NATIVE_THOUGHT"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"FAILED_NATIVE_SIGNATURE"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"FAILED_NATIVE_TEXT"}}),
        ]
    };
    for profile in [
        NativeReplayProfile::Recover,
        NativeReplayProfile::Exhaust,
        NativeReplayProfile::Committed,
    ] {
        server.reset().await;
        let (budget, replies, retried_ids, expected_success) = match profile {
            NativeReplayProfile::Recover => {
                let mut provider_error = pending("pending-1");
                provider_error.push(json!({"type":"error","error":{"type":"api_error","message":"synthetic retry"}}));
                let completed = vec![
                    json!({"type":"message_start","message":{"id":"recovered","usage":{"input_tokens":1,"output_tokens":0}}}),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Recovered native answer."}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
                    json!({"type":"message_stop"}),
                ];
                (
                    2,
                    vec![pending("pending-0"), provider_error, completed],
                    vec!["pending-0", "pending-1"],
                    true,
                )
            }
            NativeReplayProfile::Exhaust => (
                1,
                vec![pending("pending-0"), pending("pending-1")],
                vec!["pending-0"],
                false,
            ),
            NativeReplayProfile::Committed => (
                1,
                vec![vec![
                    json!({"type":"message_start","message":{"id":"committed","usage":{"input_tokens":1,"output_tokens":0}}}),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"Completed thought.","signature":"committed-signature/=="}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"one-action","name":"functions__update_plan","input":{"arguments":{"plan":[{"step":"Native commit once","status":"completed"}]}}}}),
                    json!({"type":"content_block_stop","index":1}),
                    json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}),
                    json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"FAILED_NATIVE_TEXT"}}),
                ]],
                Vec::new(),
                false,
            ),
        };
        let expected_requests = replies.len();
        let replies = replies
            .into_iter()
            .map(|frames| {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        frames
                            .into_iter()
                            .map(|frame| format!("data: {frame}\n\n"))
                            .collect::<String>(),
                    )
            })
            .collect::<Vec<_>>();
        let index = AtomicUsize::new(0);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(move |_: &MockRequest| {
                replies
                    .get(index.fetch_add(1, Ordering::SeqCst))
                    .cloned()
                    .unwrap_or_else(|| ResponseTemplate::new(500))
            })
            .expect(u64::try_from(expected_requests)?)
            .mount(server)
            .await;

        let mut info = boundary.info().clone();
        info.request_max_retries = Some(0);
        info.stream_max_retries = Some(budget);
        let provider = Arc::new(NativeBoundary {
            inner: create_model_provider_with_anthropic_account(
                info.clone(),
                // Native providers hide ChatGPT auth; reuse the fixture's runtime owner.
                Some(Arc::clone(runtime)),
                Some(boundary.account_b.clone()),
            ),
            endpoint: boundary.endpoint.clone(),
            store: AccountStore::new(home.to_path_buf()),
            account_a: boundary.account_a.clone(),
            account_b: boundary.account_b.clone(),
            switch_once: AtomicBool::new(false),
            client: Mutex::new(Weak::new()),
        });
        let model_slug = model.slug.clone();
        let (mut session, mut turn, events) =
            make_session_and_context_with_auth_config_home_and_rx(
                CodexAuth::Headers(AuthHeaders::new(http::HeaderMap::new())),
                Vec::new(),
                home,
                move |config| {
                    config.model = Some(model_slug);
                    config.model_provider = info;
                    config.model_post_turn_compact_threshold_percent = 0;
                    config.update_plan_enabled = true;
                    let _ = config.features.disable(Feature::TokenBudget);
                    let _ = config.features.disable(Feature::CodeMode);
                    let _ = config.features.disable(Feature::CodeModeHost);
                },
            )
            .await;
        let mut client = test_model_client_with_thread_id(session.thread_id, SessionSource::Exec);
        Arc::get_mut(&mut client.state).unwrap().provider = provider.clone();
        client.commit_provider_account(ProviderAccountSelection::ClaudeSubscription {
            id: boundary.account_b.clone(),
        });
        bind_model_client_for_tests(&mut session, &mut turn, client, provider, model.clone());
        session
            .record_conversation_items(&turn, model, history)
            .await;
        while events.try_recv().is_ok() {}
        session
            .spawn_task(
                Arc::clone(&turn),
                vec![TurnInput::UserInput {
                    metadata: Default::default(),
                    client_id: None,
                    content: vec![UserInput::Text {
                        text: "continue native replay".into(),
                        text_elements: Vec::new(),
                    }],
                }],
                RegularTask::new(),
            )
            .await;
        let observed = tokio::time::timeout(Duration::from_secs(30), async {
            let mut observed = Vec::new();
            loop {
                let event = events.recv().await.expect("regular task event channel");
                let terminal = matches!(&event.msg, EventMsg::TurnComplete(_));
                observed.push(event.msg);
                if terminal {
                    return observed;
                }
            }
        })
        .await?;
        let discarded = observed
            .iter()
            .enumerate()
            .filter_map(|(position, event)| {
                if let EventMsg::StreamItemsDiscarded(event) = event {
                    let mut ids = event.item_ids.clone();
                    ids.sort();
                    Some((position, ids))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            discarded.len(),
            retried_ids.len(),
            "native retry lifecycle: terminal={:?}, provider POSTs={}",
            observed.last(),
            requests.len(),
        );
        for ((position, ids), attempt) in discarded.iter().zip(&retried_ids) {
            let expected = vec![format!("{attempt}_0"), format!("{attempt}_1")];
            assert_eq!(
                *ids, expected,
                "discard every started native item exactly once"
            );
            for id in ids {
                let started = observed.iter().position(|event| {
                    matches!(event, EventMsg::ItemStarted(event) if event.item.id() == *id)
                }).expect("native item really started");
                assert!(started < *position);
                let delta = observed
                    .iter()
                    .position(|event| match event {
                        EventMsg::ReasoningContentDelta(event) => {
                            event.item_id == *id && event.delta == "FAILED_NATIVE_THOUGHT"
                        }
                        EventMsg::AgentMessageContentDelta(event) => {
                            event.item_id == *id && event.delta == "FAILED_NATIVE_TEXT"
                        }
                        _ => false,
                    })
                    .expect("native partial output really delivered");
                assert!(started < delta && delta < *position);
            }
            let next = if *attempt == "pending-0" {
                "pending-1_0"
            } else {
                "recovered_0"
            };
            let next_started = observed.iter().position(|event| {
                matches!(event, EventMsg::ItemStarted(event) if event.item.id() == next)
            }).expect("retried native item really started");
            assert!(*position < next_started);
        }
        let EventMsg::TurnComplete(completed) = observed.last().unwrap() else {
            unreachable!("loop stops at turn completion");
        };
        assert_eq!(completed.error.is_none(), expected_success);
        assert_eq!(requests.len(), expected_requests);
        let bodies = requests
            .iter()
            .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body).unwrap())
            .collect::<Vec<_>>();
        for body in &bodies {
            let messages = &body["messages"];
            let wire = messages.to_string();
            assert!(wire.contains("retained-signature/=="));
            assert!(wire.contains("retain prior thought"));
            assert!(wire.contains("exec-call"));
            assert!(wire.contains("result λ"));
            assert!(wire.contains("continue native replay"));
            assert!(!wire.contains("FAILED_NATIVE_"));
            assert_eq!(messages.as_array().unwrap().last().unwrap()["role"], "user");
            assert_eq!(
                *messages, bodies[0]["messages"],
                "retry contains only committed history"
            );
        }
        let committed = matches!(profile, NativeReplayProfile::Committed);
        assert_eq!(observed.iter().filter(|event| {
            matches!(event, EventMsg::PlanUpdate(event) if event.plan.len() == 1 && event.plan[0].step == "Native commit once")
        }).count(), usize::from(committed));
        if committed {
            assert!(
                bodies[0]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["name"] == "functions__update_plan")
            );
            let history = session
                .clone_history()
                .await
                .for_prompt(&model.input_modalities);
            assert_eq!(
                history
                    .iter()
                    .filter(|item| {
                        matches!(item, ResponseItem::FunctionCallOutput { call_id, output, .. }
                    if call_id.as_deref() == Some("one-action") && output.success == Some(true))
                    })
                    .count(),
                1
            );
            assert!(history.iter().any(|item| {
                matches!(item, ResponseItem::Reasoning { encrypted_content: Some(signature), .. }
                    if signature == "committed-signature/==")
            }));
        }
        server.verify().await;
    }
    Ok(())
}

#[derive(Default)]
struct WarmCapture {
    lore: Mutex<Vec<crate::WarmingLore>>,
    replay: Mutex<Option<crate::WarmingRequest>>,
}

impl crate::PromptCacheObserver for WarmCapture {
    fn epoch(&self, _thread: ThreadId) -> u64 {
        0
    }
    fn fact(&self, _thread: ThreadId, _fact: crate::PromptCacheFact) {}
    fn completed(&self, _thread: ThreadId, request: crate::CompletedRequest) {
        self.lore.lock().unwrap().push(request.lore);
        if let Ok(replay) = request.replay {
            *self.replay.lock().unwrap() = Some(replay);
        }
    }
}

impl std::fmt::Debug for WarmCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WarmCapture").finish_non_exhaustive()
    }
}
