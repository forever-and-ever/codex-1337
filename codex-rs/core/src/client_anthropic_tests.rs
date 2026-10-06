use super::*;
use crate::agent::control::ProviderAccountSelection;
use codex_login::AuthHeaders;
use codex_login::anthropic::AccountIdentity;
use codex_login::anthropic::AccountInfo;
use codex_login::anthropic::AccountStore;
use codex_login::anthropic::SubscriptionGrant;
use codex_model_provider::AnthropicAuthContext;
use codex_model_provider::create_model_provider_with_anthropic_account;
use codex_model_provider_info::ModelProviderAdapter;
use codex_protocol::openai_models::AnthropicModelCapabilities;
use codex_protocol::openai_models::AnthropicThinkingMode;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
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
async fn native_request_pins_recovery_then_switches_with_exact_custom_tool_replay()
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
        .respond_with(ResponseTemplate::new(/*status*/ 401))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let custom_input = "text('λ <&>');\n";
    let tool_frames = [
        json!({"type":"message_start","message":{"id":"msg-a","usage":{"input_tokens":3,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"exec-call","name":"functions__exec","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":json!({"input":custom_input}).to_string()}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}),
        json!({"type":"message_stop"}),
    ];
    let done_frames = [
        json!({"type":"message_start","message":{"id":"msg-b","usage":{"input_tokens":5,"output_tokens":0}}}),
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
            .respond_with(
                ResponseTemplate::new(/*status*/ 200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        frames
                            .iter()
                            .map(|frame| format!("data: {frame}\n\n"))
                            .collect::<String>(),
                    ),
            )
            .expect(/*requests*/ 1)
            .mount(&server)
            .await;
    }
    // Empty headers supply runtime policy/home only, not an OpenAI credential.
    let runtime = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::Headers(AuthHeaders::new(http::HeaderMap::new())),
        home.path().to_path_buf(),
    );
    let mut client = test_model_client(SessionSource::Cli);
    let mut info = client.state.provider.info().clone();
    info.adapter = ModelProviderAdapter::ClaudeSubscription;
    info.base_url = None;
    let boundary = Arc::new(NativeBoundary {
        inner: create_model_provider_with_anthropic_account(
            info,
            Some(runtime),
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
    let client = Arc::new(client);
    *boundary.client.lock().unwrap() = Arc::downgrade(&client);
    client.commit_provider_account(ProviderAccountSelection::ClaudeSubscription {
        id: id_a.to_string(),
    });
    let mut model = test_model_info();
    model.default_reasoning_level = None;
    model.anthropic = Some(AnthropicModelCapabilities {
        thinking: AnthropicThinkingMode::Disabled,
        max_output_tokens: std::num::NonZeroU32::new(/*n*/ 100).unwrap(),
        supported_efforts: vec![],
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
    let ResponseItem::CustomToolCall { id, .. } = &mut calls[0] else {
        panic!("expected custom call");
    };
    *id = None;
    assert_eq!(calls, vec![expected_call]);
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
            /*effort*/ None,
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
        for message in body["messages"].as_array_mut().unwrap() {
            for block in message["content"].as_array_mut().unwrap() {
                block.as_object_mut().unwrap().remove("cache_control");
            }
        }
        json!({"bearer":request.headers["authorization"].to_str().unwrap(), "messages":body["messages"]})
    }).collect::<Vec<_>>();
    let user = json!({"role":"user","content":[{"type":"text","text":"run"}]});
    assert_eq!(
        observed,
        vec![
            json!({"bearer":"Bearer original-a","messages":[user]}),
            json!({"bearer":"Bearer rotated-a","messages":[user]}),
            json!({"bearer":"Bearer bearer-b","messages":[user,
            {"role":"assistant","content":[{"type":"tool_use","id":"exec-call","name":"functions__exec","input":{"input":custom_input}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"exec-call","content":"result λ","is_error":false}]}]}),
        ]
    );
    Ok(())
}
