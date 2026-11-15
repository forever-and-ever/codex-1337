#![allow(clippy::unwrap_used)]

use codex_features::Feature;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::models::PermissionProfile;
use core_test_support::mock_server;
use core_test_support::responses;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

fn find_web_search_tool(body: &Value) -> &Value {
    body["tools"]
        .as_array()
        .expect("request body should include tools array")
        .iter()
        .find(|tool| tool.get("type").and_then(Value::as_str) == Some("web_search"))
        .expect("tools should include a web_search tool")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_search_mode_cached_sets_external_web_access_false() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let sse = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_completed("resp-1"),
    ]);
    let resp_mock = responses::mount_sse_once(&server, sse).await;

    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config
            .web_search_mode
            .set(WebSearchMode::Cached)
            .expect("test web_search_mode should satisfy constraints");
    });
    let test = builder
        .build(&server)
        .await
        .expect("create test Codex conversation");

    test.submit_turn_with_permission_profile(
        "hello cached web search",
        PermissionProfile::read_only(),
    )
    .await
    .expect("submit turn");

    let body = resp_mock.single_request().body_json();
    let tool = find_web_search_tool(&body);
    assert_eq!(
        tool.get("external_web_access").and_then(Value::as_bool),
        Some(false),
        "web_search cached mode should force external_web_access=false"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_search_mode_takes_precedence_over_legacy_flags() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let sse = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_completed("resp-1"),
    ]);
    let resp_mock = responses::mount_sse_once(&server, sse).await;

    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config
            .features
            .enable(Feature::WebSearchRequest)
            .expect("test config should allow feature update");
        config
            .web_search_mode
            .set(WebSearchMode::Cached)
            .expect("test web_search_mode should satisfy constraints");
    });
    let test = builder
        .build(&server)
        .await
        .expect("create test Codex conversation");

    test.submit_turn_with_permission_profile(
        "hello cached+live flags",
        PermissionProfile::read_only(),
    )
    .await
    .expect("submit turn");

    let body = resp_mock.single_request().body_json();
    let tool = find_web_search_tool(&body);
    assert_eq!(
        tool.get("external_web_access").and_then(Value::as_bool),
        Some(false),
        "web_search mode should win over legacy web_search_request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_search_mode_defaults_to_cached_when_features_disabled() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let sse = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_completed("resp-1"),
    ]);
    let resp_mock = responses::mount_sse_once(&server, sse).await;

    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config
            .web_search_mode
            .set(WebSearchMode::Cached)
            .expect("test web_search_mode should satisfy constraints");
        config
            .features
            .disable(Feature::WebSearchCached)
            .expect("test config should allow feature update");
        config
            .features
            .disable(Feature::WebSearchRequest)
            .expect("test config should allow feature update");
    });
    let test = builder
        .build(&server)
        .await
        .expect("create test Codex conversation");

    test.submit_turn_with_permission_profile(
        "hello default cached web search",
        PermissionProfile::read_only(),
    )
    .await
    .expect("submit turn");

    let body = resp_mock.single_request().body_json();
    let tool = find_web_search_tool(&body);
    assert_eq!(
        tool.get("external_web_access").and_then(Value::as_bool),
        Some(false),
        "default web_search should be cached when unset"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_search_mode_updates_between_turns_with_permission_profile() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let resp_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config
            .web_search_mode
            .set(WebSearchMode::Cached)
            .expect("test web_search_mode should satisfy constraints");
        config
            .features
            .disable(Feature::WebSearchCached)
            .expect("test config should allow feature update");
        config
            .features
            .disable(Feature::WebSearchRequest)
            .expect("test config should allow feature update");
    });
    let test = builder
        .build(&server)
        .await
        .expect("create test Codex conversation");

    test.submit_turn_with_permission_profile("hello cached", PermissionProfile::read_only())
        .await
        .expect("submit first turn");
    test.submit_turn_with_permission_profile("hello live", PermissionProfile::Disabled)
        .await
        .expect("submit second turn");

    let requests = resp_mock.requests();
    assert_eq!(requests.len(), 2, "expected two response requests");

    let first_body = requests[0].body_json();
    let first_tool = find_web_search_tool(&first_body);
    assert_eq!(
        first_tool
            .get("external_web_access")
            .and_then(Value::as_bool),
        Some(false),
        "read-only policy should default web_search to cached"
    );

    let second_body = requests[1].body_json();
    let second_tool = find_web_search_tool(&second_body);
    assert_eq!(
        second_tool
            .get("external_web_access")
            .and_then(Value::as_bool),
        Some(true),
        "danger-full-access policy should default web_search to live"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_search_tool_config_from_config_toml_is_forwarded_to_request() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let sse = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_completed("resp-1"),
    ]);
    let resp_mock = responses::mount_sse_once(&server, sse).await;

    let home = Arc::new(tempfile::TempDir::new().expect("create codex home"));
    std::fs::write(
        home.path().join("config.toml"),
        r#"web_search = "live"

[tools.web_search]
context_size = "high"
allowed_domains = ["example.com"]
location = { country = "US", city = "New York", timezone = "America/New_York" }
"#,
    )
    .expect("write config.toml");

    let mut builder = test_codex().with_model("gpt-5.2").with_home(home);
    let test = builder
        .build(&server)
        .await
        .expect("create test Codex conversation");

    test.submit_turn_with_permission_profile(
        "hello configured web search",
        PermissionProfile::Disabled,
    )
    .await
    .expect("submit turn");

    let body = resp_mock.single_request().body_json();
    let tool = find_web_search_tool(&body);
    assert_eq!(
        tool,
        &json!({
            "type": "web_search",
            "external_web_access": true,
            "search_context_size": "high",
            "filters": {
                "allowed_domains": ["example.com"],
            },
            "user_location": {
                "type": "approximate",
                "country": "US",
                "city": "New York",
                "timezone": "America/New_York",
            },
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn indexed_web_search_mode_sets_indexed_access() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let sse = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_completed("resp-1"),
    ]);
    let resp_mock = responses::mount_sse_once(&server, sse).await;

    let home = Arc::new(tempfile::TempDir::new().expect("create codex home"));
    std::fs::write(home.path().join("config.toml"), r#"web_search = "indexed""#)
        .expect("write config.toml");

    let mut builder = test_codex().with_model("gpt-5.2").with_home(home);
    let test = builder
        .build(&server)
        .await
        .expect("create test Codex conversation");

    test.submit_turn_with_permission_profile(
        "hello indexed web search",
        PermissionProfile::Disabled,
    )
    .await
    .expect("submit turn");

    let body = resp_mock.single_request().body_json();
    let tool = find_web_search_tool(&body);
    assert_eq!(
        (
            tool.get("external_web_access").and_then(Value::as_bool),
            tool.get("indexed_web_access").and_then(Value::as_bool),
        ),
        (Some(true), Some(true))
    );
}

#[tokio::test]
async fn claude_standalone_search_borrows_chatgpt() -> anyhow::Result<()> {
    assert_claude_standalone_search(SearchLogin::Chatgpt).await
}

#[tokio::test]
async fn claude_standalone_search_refuses_api_key_billing() -> anyhow::Result<()> {
    assert_claude_standalone_search(SearchLogin::ApiKeyOnly).await
}

#[derive(Clone, Copy)]
enum SearchLogin {
    Chatgpt,
    ApiKeyOnly,
}

async fn assert_claude_standalone_search(login: SearchLogin) -> anyhow::Result<()> {
    use codex_extension_api::ConversationHistory;
    use codex_extension_api::ExtensionData;
    use codex_extension_api::ExtensionRegistryBuilder;
    use codex_extension_api::FunctionCallError;
    use codex_extension_api::NoopTurnItemEmitter;
    use codex_extension_api::ToolCall;
    use codex_extension_api::ToolCallSource;
    use codex_extension_api::ToolName;
    use codex_extension_api::ToolPayload;
    use codex_http_client::HttpClientFactory;
    use codex_http_client::NetworkPolicy;
    use codex_http_client::OutboundProxyPolicy;
    use codex_login::AuthCredentialsStoreMode;
    use codex_login::AuthKeyringBackendKind;
    use codex_login::AuthManager;
    use codex_login::AuthRouteConfig;
    use codex_login::CodexAuth;
    use codex_model_provider_info::ModelProviderAdapter;
    use codex_model_provider_info::ModelProviderInfo;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::openai_models::AnthropicModelCapabilities;
    use codex_protocol::openai_models::AnthropicThinkingMode;
    use codex_protocol::openai_models::ModelVisibility;
    use codex_utils_output_truncation::TruncationPolicy;
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    skip_if_no_network!(Ok(()));
    let server = wiremock::MockServer::start().await;
    mock_server::run(&[&server], async {
    let home = Arc::new(tempfile::TempDir::new()?);
    let policy = NetworkPolicy::default().restrict_to_endpoints(
        [
            format!(
                "{}/v1/models?client_version={}",
                server.uri(),
                codex_models_manager::client_version_to_whole()
            )
            .parse()?,
            format!("{}/v1/alpha/search", server.uri()).parse()?,
        ]
        .into(),
    );
    let auth = match login {
        SearchLogin::Chatgpt => {
            std::fs::write(
                home.path().join("auth.json"),
                serde_json::to_vec(&json!({
                    "auth_mode": "chatgpt",
                    "tokens": {
                        "id_token": "e30.e30.signature",
                        "access_token": "search-chatgpt-token",
                        "refresh_token": "search-refresh-token",
                        "account_id": "search-chatgpt-account"
                    },
                    "last_refresh": chrono::Utc::now()
                }))?,
            )?;
            AuthManager::shared(
                home.path().to_path_buf(),
                /*enable_codex_api_key_env*/ false,
                AuthCredentialsStoreMode::File,
                /*forced_chatgpt_workspace_id*/ None,
                /*chatgpt_base_url*/ None,
                AuthKeyringBackendKind::default(),
                AuthRouteConfig::from_http_client_factory(
                    HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
                        .with_network_policy(policy.clone()),
                ),
            )
            .await
        }
        SearchLogin::ApiKeyOnly => codex_core::test_support::auth_manager_from_auth_with_home(
            CodexAuth::from_api_key("must-not-bill"),
            home.path().to_path_buf(),
        ),
    };
    let mut openai_catalog = codex_models_manager::bundled_models_response()?;
    openai_catalog.models.truncate(1);
    openai_catalog.models[0].slug = "openai-search-default".into();
    openai_catalog.models[0].visibility = ModelVisibility::List;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&openai_catalog))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/alpha/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "encrypted_output": "ciphertext", "output": "Search result"
        })))
        .expect(match login {
            SearchLogin::Chatgpt => 1,
            SearchLogin::ApiKeyOnly => 0,
        })
        .mount(&server)
        .await;
    let mut claude_catalog = openai_catalog;
    claude_catalog.models[0].slug = "claude-search-fixture".into();
    claude_catalog.models[0].anthropic = Some(AnthropicModelCapabilities {
        thinking: AnthropicThinkingMode::Disabled,
        max_output_tokens: std::num::NonZeroU32::new(8192).unwrap(),
        supported_efforts: Vec::new(),
        supports_structured_outputs: false,
    });
    let mut registry = ExtensionRegistryBuilder::new();
    codex_web_search_extension::install(&mut registry, auth.clone());
    let registry = Arc::new(registry.build());
    let extensions = registry.clone();
    // Native subscription endpoints are intentionally not redirectable. Build a real
    // Claude thread, but invoke its executor without sending Claude inference.
    let mut builder = test_codex()
        .with_home(home)
        .with_auth_manager(auth)
        .with_extensions(registry)
        .with_config(move |config| {
            config.application_network_policy = policy;
            config
                .model_providers
                .insert("openai".into(), config.model_provider.clone());
            config.model_provider_id = "claude_subscription".into();
            config.model_provider = ModelProviderInfo {
                name: "Claude subscription".into(),
                adapter: ModelProviderAdapter::ClaudeSubscription,
                ..Default::default()
            };
            config.model_providers.insert(
                config.model_provider_id.clone(),
                config.model_provider.clone(),
            );
            config.model = Some("claude-search-fixture".into());
            config.model_catalog = Some(claude_catalog);
            config.web_search_mode.set(WebSearchMode::Live).unwrap();
            config
                .features
                .disable(Feature::StandaloneWebSearch)
                .unwrap();
        });
    let test = builder.build_with_auto_env(&server).await?;
    let session_store = ExtensionData::new("search-session");
    let step_store = ExtensionData::new("search-step");
    step_store.insert(WebSearchMode::Live);
    let executor = extensions
        .tool_contributors()
        .iter()
        .flat_map(|contributor| {
            contributor.tools_for_step(&session_store, test.codex.thread_extension_data(), &step_store)
        })
        .find(|tool| tool.tool_name() == ToolName::namespaced("web", "run"))
        .expect("Claude thread contributes web.run");
    let history: Vec<ResponseItem> = serde_json::from_value(json!([
        {"type":"message","role":"user","content":[{"type":"input_text","text":"Previous user"}]},
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Previous Claude answer"}]},
        {"type":"message","role":"user","content":[{"type":"input_text","text":"Search the web"}]}
    ]))?;
    let payload = ToolPayload::Function {
        arguments: json!({"search_query":[{"q":"standalone web search"}]}).to_string(),
    };
    let result = executor
        .handle(ToolCall {
            turn_id: "search-turn".into(),
            call_id: "search-call".into(),
            tool_name: ToolName::namespaced("web", "run"),
            model: "claude-search-fixture".into(),
            codex_turn_metadata: None,
            truncation_policy: TruncationPolicy::Tokens(2000),
            source: ToolCallSource::Direct,
            conversation_history: ConversationHistory::new(history.clone()),
            turn_item_emitter: Arc::new(NoopTurnItemEmitter),
            attachment_store: Arc::new(codex_attachment_store::InlineAttachmentStore),
            ephemeral: false,
            environments: Vec::new(),
            payload: payload.clone(),
        })
        .await;
    match login {
        SearchLogin::Chatgpt => {
            let output = result?;
            assert_eq!(
                serde_json::to_value(output.to_response_item("search-call", &payload))?,
                json!({"type":"function_call_output","call_id":"search-call",
                    "output":[{"type":"input_text","text":"Search result"}]})
            );
            let requests = server.received_requests().await.unwrap();
            let search = requests
                .iter()
                .find(|r| r.url.path() == "/v1/alpha/search")
                .expect("delegated search");
            assert_eq!(
                (
                    search.headers["authorization"].to_str()?,
                    search.headers["chatgpt-account-id"].to_str()?
                ),
                ("Bearer search-chatgpt-token", "search-chatgpt-account"),
            );
            assert!(
                requests
                    .iter()
                    .any(|request| request.url.path() == "/v1/models")
            );
            let body = search.body_json::<Value>()?;
            assert_eq!(
                (&body["model"], &body["input"]),
                (
                    &json!("openai-search-default"),
                    &serde_json::to_value(history)?
                )
            );
        }
        SearchLogin::ApiKeyOnly => {
            assert!(
                matches!(result, Err(FunctionCallError::RespondToModel(message))
                if message == "Web search for Claude conversations needs an active ChatGPT login; choose one with /account.")
            );
            assert!(server.received_requests().await.unwrap().is_empty());
        }
    }
    test.codex.shutdown_and_wait().await?;
    server.verify().await;
    Ok(())
    })
    .await
}
