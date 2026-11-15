use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_model_provider_info::WireApi;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header_regex;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;

fn test_codex_home() -> PathBuf {
    std::env::temp_dir().join(format!("codex-model-provider-test-{}", std::process::id()))
}

fn provider_for(base_url: String) -> ModelProviderInfo {
    ModelProviderInfo {
        adapter: codex_model_provider_info::ModelProviderAdapter::OpenaiCompatible,
        name: "mock".into(),
        base_url: Some(base_url),
        model_catalog_url: None,
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        gateway_oauth: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        stream_idle_timeout_ms: Some(5_000),
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_standalone_web_search: false,
        capabilities: None,
        include_internal_metadata: false,
    }
}

fn remote_model(slug: &str) -> ModelInfo {
    serde_json::from_value(json!({
        "slug": slug,
        "display_name": slug,
        "description": null,
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 0,
        "upgrade": null,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10_000},
        "supports_image_detail_original": false,
        "context_window": 272_000,
        "max_context_window": 272_000,
        "experimental_supported_tools": [],
    }))
    .expect("valid model")
}

struct RotatingAuth(AtomicUsize);

impl codex_login::ExternalAuth for RotatingAuth {
    fn resolve(&self) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async move {
            let key = if self.0.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
                "sk-proj-fedramp-synthetic-test-key"
            } else {
                "commercial-key"
            };
            Ok(CodexAuth::from_api_key(key))
        })
    }

    fn refresh(
        &self,
        _context: codex_login::ExternalAuthRefreshContext,
    ) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        self.resolve()
    }
}

#[tokio::test]
async fn configured_provider_resolves_default_and_explicit_endpoints() {
    let provider = create_model_provider(
        provider_for("https://example.test/v1".to_string()),
        /*auth_manager*/ None,
    );

    assert_eq!(
        provider
            .runtime_base_url()
            .await
            .expect("runtime base URL should resolve"),
        Some("https://example.test/v1".to_string())
    );
    assert_eq!(
        provider
            .request_provider_and_auth(None)
            .await
            .unwrap()
            .0
            .base_url,
        "https://example.test/v1"
    );

    let home = tempfile::tempdir().unwrap();
    let auth = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("initial-key"),
        home.path().to_path_buf(),
    );
    let provider = create_model_provider(
        ModelProviderInfo::create_openai_provider(/*base_url*/ None),
        Some(auth.clone()),
    );
    for (api_key, expected_base_url) in [
        ("legacy-key", "https://api.openai.com/v1"),
        (
            "sk-proj-fedramp-synthetic-test-key",
            "https://gov.api.openai.com/v1",
        ),
        ("commercial-key", "https://api.openai.com/v1"),
    ] {
        codex_login::login_with_api_key(
            home.path(),
            api_key,
            codex_login::AuthCredentialsStoreMode::File,
            codex_login::AuthKeyringBackendKind::default(),
        )
        .unwrap();
        auth.reload().await;
        let (api_provider, api_auth) = provider.request_provider_and_auth(None).await.unwrap();
        assert_eq!(api_provider.base_url, expected_base_url);
        let headers = api_auth.auth.to_auth_headers().unwrap();
        assert_eq!(
            headers[http::header::AUTHORIZATION].to_str().unwrap(),
            format!("Bearer {api_key}")
        );
    }
    assert_eq!(provider.info().base_url, None);

    let rotating_auth = Arc::new(RotatingAuth(AtomicUsize::new(0)));
    auth.set_external_auth(rotating_auth.clone()).await.unwrap();
    for (api_key, expected_base_url, expected_fedramp_header) in [
        ("commercial-key", "https://api.openai.com/v1", None),
        (
            "sk-proj-fedramp-synthetic-test-key",
            "https://gov.api.openai.com/v1",
            Some("true"),
        ),
        ("commercial-key", "https://api.openai.com/v1", None),
    ] {
        let reads_before = rotating_auth.0.load(Ordering::SeqCst);
        let (api_provider, api_auth) = provider.request_provider_and_auth(None).await.unwrap();
        assert_eq!(rotating_auth.0.load(Ordering::SeqCst), reads_before + 1);
        let headers = api_auth.auth.to_auth_headers().unwrap();
        let expected_authorization = format!("Bearer {api_key}");
        assert_eq!(
            (
                api_provider.base_url.as_str(),
                headers
                    .get(http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                headers
                    .get("X-OpenAI-Fedramp")
                    .and_then(|value| value.to_str().ok()),
            ),
            (
                expected_base_url,
                Some(expected_authorization.as_str()),
                expected_fedramp_header,
            )
        );
    }
}

#[tokio::test]
async fn configured_provider_models_manager_uses_provider_bearer_token() {
    let server = MockServer::start().await;
    let remote_models = vec![remote_model("provider-model")];

    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header_regex("Authorization", "Bearer provider-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(ModelsResponse {
                    models: remote_models.clone(),
                }),
        )
        .expect(2)
        .mount(&server)
        .await;

    let mut provider_info = provider_for(server.uri());
    provider_info.experimental_bearer_token = Some("provider-token".into());
    provider_info.model_catalog_url = Some(format!("{}/models", server.uri()).into());
    provider_info.http_headers = Some(std::collections::HashMap::from([(
        codex_login::default_client::RESIDENCY_HEADER_NAME.to_string(),
        "us".into(),
    )]));
    for auth in [
        None,
        Some(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
    ] {
        // Disabled discovery must ignore the catalog cached by the enabled run.
        for enabled in [true, false] {
            let provider = create_model_provider(
                provider_info.clone(),
                auth.clone().map(AuthManager::from_auth_for_testing),
            );
            let manager = provider.models_manager(
                test_codex_home().join("models_cache.json"),
                /*config_model_catalog*/ None,
            );
            manager.set_api_key_model_discovery_enabled(enabled);
            let refresh_strategy = if enabled {
                RefreshStrategy::Online
            } else {
                RefreshStrategy::Offline
            };
            let catalog = manager
                .raw_model_catalog(
                    refresh_strategy,
                    HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
                )
                .await;

            assert_eq!(
                catalog
                    .models
                    .iter()
                    .any(|model| model.slug == "provider-model"),
                enabled
            );
        }
    }
}
