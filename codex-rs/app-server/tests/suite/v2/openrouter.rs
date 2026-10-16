use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ErrorNotification;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::OpenRouterKeyAddResponse;
use codex_app_server_protocol::OpenRouterKeyState;
use codex_app_server_protocol::OpenRouterKeyStatusResponse;
use codex_app_server_protocol::OpenRouterModelEndpointsResponse;
use codex_app_server_protocol::OpenRouterModelRouteResponse;
use codex_app_server_protocol::OpenRouterModelSaveResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_protocol::Usd;
use core_test_support::mock_server;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

const READ_TIMEOUT: Duration = Duration::from_secs(30);

// The public configuration path must reach the same catalog, payer and routing
// used by a live turn; checking only the request handlers cannot prove that.
#[tokio::test]
async fn openrouter_saved_model_routes_spend_and_key_quota() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    mock_server::run(&[&server], async {
        let key_path = home.path().join("openrouter.key");
        std::fs::write(&key_path, "sk-or-test\n")?;
        let config_path = home.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"model_provider = "openrouter"
model = "moonshotai/kimi-test"
approval_policy = "never"
sandbox_mode = "danger-full-access"
[model_providers.openrouter]
name = "OpenRouter"
adapter = "openrouter"
base_url = {}
requires_openai_auth = false
supports_websockets = false
request_max_retries = 0
"#,
                serde_json::to_string(&server.uri())?,
            ),
        )?;
        Mock::given(method("GET"))
            .and(path("/key"))
            .and(header("authorization", "Bearer sk-or-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
                "label": "…test", "limit": 20, "limit_remaining": 18,
                "limit_reset": "monthly", "usage_daily": 0.25, "usage_weekly": 1,
                "usage_monthly": 2, "expires_at": "2030-01-01T00:00:00Z"
            }})))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/credits"))
            .and(header("authorization", "Bearer sk-or-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data": {"total_credits": 30, "total_usage": 2}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/model/moonshotai/kimi-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
                "id": "moonshotai/kimi-test", "name": "MoonshotAI: Kimi Test",
                "context_length": 262144, "architecture": {"input_modalities": ["text"]},
                "supported_parameters": ["tools"], "pricing": {"prompt": "0.0000003", "completion": "0.000002"}
            }})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/models/moonshotai/kimi-test/endpoints"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"endpoints": [{
                "tag": "moonshotai/turbo", "provider_name": "Moonshot AI",
                "quantization": "fp8", "service_tier": "priority", "context_length": 131072,
                "pricing": {"prompt": "0.0000003", "completion": "0.000002"},
                "uptime_last_30m": 99.5, "status": 0
            }]}})))
            .mount(&server)
            .await;
        let mut completed = responses::ev_completed_with_tokens("paid-turn", 123);
        completed["response"]["usage"]["cost"] = json!(0.0123);
        let wire = responses::mount_response_sequence(
            &server,
            vec![
                responses::sse_response(responses::sse(vec![
                    responses::ev_response_created("paid-turn"),
                    completed,
                ])),
                ResponseTemplate::new(402).set_body_json(json!({
                    "error": {"message": "Insufficient credits", "code": 402}
                })),
            ],
        )
        .await;
        let mut app = TestAppServer::builder()
            .with_codex_home(home.path())
            .with_env_overrides(&[("OPENAI_API_KEY", None)])
            .build_initialized_with_timeout(READ_TIMEOUT)
            .await?;

        let request = app.send_request("openrouter/key/add", Some(json!({
            "name": "main", "path": key_path
        }))).await?;
        let added: OpenRouterKeyAddResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(added.key.name.as_str(), "main");
        assert!(added.key.is_default);
        assert_eq!(added.key.path.as_path(), key_path);
        for _ in 0..2 {
            let request = app.send_request("openrouter/key/status", Some(json!({}))).await?;
            let status: OpenRouterKeyStatusResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
            let [status] = status.keys.as_slice() else { anyhow::bail!("expected one key"); };
            assert!(matches!(&status.state, OpenRouterKeyState::Read {
                balance: Some(balance), expires_at: Some(1_893_456_000), ..
            } if *balance == Usd::from_nanodollars(28_000_000_000)));
        }

        let request = app.send_request("openrouter/model/save", Some(json!({
            "slug": "moonshotai/kimi-test"
        }))).await?;
        let saved: OpenRouterModelSaveResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(saved.model.model, "moonshotai/kimi-test");
        let request = app.send_thread_start_request_with_auto_env(ThreadStartParams::default()).await?;
        let started: ThreadStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let request = app.send_request("thread/account/list", Some(json!({"threadId": started.thread.id}))).await?;
        let accounts: codex_app_server_protocol::ThreadAccountListResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(accounts.conversation_provider, Some(codex_app_server_protocol::AccountProvider::OpenRouter));
        assert_eq!(accounts.selected_openrouter_key.as_ref().map(|name| name.as_str()), Some("main"));
        assert_eq!(accounts.pending_openrouter_key, None);
        assert_eq!(accounts.openrouter_keys.len(), 1);
        let request = app.send_request("openrouter/model/endpoints", Some(json!({
            "slug": "moonshotai/kimi-test"
        }))).await?;
        let endpoints: OpenRouterModelEndpointsResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(endpoints.hosts[0].slug.as_str(), "moonshotai");
        assert_eq!(endpoints.hosts[0].endpoints[0].prompt_per_mtok, Some(Usd::from_nanodollars(300_000_000)));
        let request = app.send_request("openrouter/model/route", Some(json!({
            "slug": "moonshotai/kimi-test", "filter": {"only": ["moonshotai"]}
        }))).await?;
        let _: OpenRouterModelRouteResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let request = app.send_request("model/list", Some(json!({"modelProvider": "openrouter"}))).await?;
        let listed: ModelListResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(listed.data.len(), 1);
        assert_eq!(listed.data[0].model, saved.model.model);
        assert!(listed.providers.iter().any(|provider| provider.id == "openrouter"));
        let stored: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
        assert_eq!(stored["wrangler"]["openrouter"]["models"]["moonshotai/kimi-test"]["only"][0].as_str(), Some("moonshotai"));
        assert!(!std::fs::read_to_string(&config_path)?.contains("sk-or-test"));

        let request = app.send_turn_start_request(TurnStartParams {
            thread_id: started.thread.id.clone(),
            input: vec![UserInput::Text { text: "Reply briefly.".into(), text_elements: Vec::new() }],
            ..Default::default()
        }).await?;
        let _: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let usage: ThreadTokenUsageUpdatedNotification = timeout(
            READ_TIMEOUT, app.read_notification("thread/tokenUsage/updated")
        ).await??;
        assert_eq!(usage.token_usage.total.cost, Some(Usd::from_nanodollars(12_300_000)));
        assert_eq!(usage.token_usage.last.cost, usage.token_usage.total.cost);
        let completed: TurnCompletedNotification = timeout(READ_TIMEOUT, app.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
        let request = wire.requests().into_iter().next().ok_or_else(|| anyhow::anyhow!("paid turn request is missing"))?;
        assert_eq!(request.header("authorization"), Some("Bearer sk-or-test".into()));
        let body = request.body_json();
        assert_eq!(body["provider"], json!({"sort": "exacto", "only": ["moonshotai"]}));
        assert_eq!(body["model"], "moonshotai/kimi-test");
        assert_eq!(body["store"], false);

        let request = app.send_turn_start_request(TurnStartParams {
            thread_id: started.thread.id,
            input: vec![UserInput::Text { text: "Use the exhausted key.".into(), text_elements: Vec::new() }],
            ..Default::default()
        }).await?;
        let _: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let error: ErrorNotification = timeout(READ_TIMEOUT, app.read_notification("error")).await??;
        assert!(!error.will_retry);
        assert_eq!(error.error.codex_error_info, Some(codex_app_server_protocol::CodexErrorInfo::UsageLimitExceeded));
        assert!(error.error.message.contains("main"));
        assert!(error.error.message.contains("/account"));
        let completed: TurnCompletedNotification = timeout(READ_TIMEOUT, app.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Failed);
        assert_eq!(wire.requests().len(), 2);
        Ok(())
    }).await
}
