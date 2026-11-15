//! Exercises live catalog routing through the public model/list RPC.

use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::LoginAccountResponse;
use codex_app_server_protocol::LogoutAccountResponse;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::ModelProviderConfigWriteResponse;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use test_case::test_case;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

fn catalog(slug: &str) -> Result<ModelsResponse> {
    Ok(ModelsResponse {
        models: vec![serde_json::from_value::<ModelInfo>(json!({
            "slug": slug,
            "display_name": slug,
            "supported_reasoning_levels": [],
            "shell_type": "shell_command",
            "visibility": "list",
            "supported_in_api": true,
            "priority": 0,
            "support_verbosity": false,
            "truncation_policy": {"mode": "bytes", "limit": 10000},
            "experimental_supported_tools": []
        }))?],
    })
}

async fn list_models(server: &mut TestAppServer) -> Result<ModelListResponse> {
    server
        .request(|request_id| ClientRequest::ModelList {
            request_id,
            params: ModelListParams::default(),
        })
        .await
}

async fn sign_in(server: &mut TestAppServer) -> Result<()> {
    let id = server
        .send_login_account_api_key_request("sk-catalog-test")
        .await?;
    let _: LoginAccountResponse = server.read_response(id).await?;
    Ok(())
}

#[test_case("other"; "selection_changes")]
#[test_case("gateway"; "definition_changes")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Switching managed provider selection or definition refreshes the catalog in
/// the same server, while invalid requirements still block catalog access.
async fn model_list_follows_changed_provider_without_restart(
    required_selection: &str,
) -> Result<()> {
    let gateway = MockServer::start().await;
    let other = MockServer::start().await;
    for (server, slug) in [(&gateway, "gateway-model"), (&other, "other-model")] {
        Mock::given(method("GET"))
            .and(path_regex(".*/models$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalog(slug)?))
            .mount(server)
            .await;
    }
    let home = TempDir::new()?;
    let providers = format!(
        r#"
[model_providers.gateway]
name = "Gateway"
base_url = "{}/v1"
model_catalog_url = "{}/v1/models"
requires_openai_auth = true
[model_providers.other]
name = "Other"
base_url = "{}/v1"
model_catalog_url = "{}/v1/models"
requires_openai_auth = true
"#,
        gateway.uri(),
        gateway.uri(),
        other.uri(),
        other.uri(),
    );
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "model_provider = 'gateway'\n[features]\napi_key_model_discovery = true\n{providers}"
        ),
    )?;
    codex_login::login_with_api_key(
        home.path(),
        "sk-initial-catalog-test",
        AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    )?;
    let requirements_path = home.path().join("requirements.toml");
    std::fs::write(
        &requirements_path,
        format!("model_provider = 'gateway'\n{providers}"),
    )?;
    let mut server = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(Duration::from_secs(/*secs*/ 60))
        .await?;

    // Let the startup refresh populate the cache before exercising cached RPCs.
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        loop {
            if let Ok(bytes) = tokio::fs::read(home.path().join("models_cache_gateway.json")).await
                && let Ok(cache) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && cache["models"][0]["slug"] == "gateway-model"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
        }
    })
    .await?;

    let initial = list_models(&mut server).await?;
    assert_eq!(
        initial
            .data
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gateway-model"]
    );
    // A provider selector cannot override a required selection, even before its catalog is cached.
    let id = server
        .send_raw_request("model/list", Some(json!({"modelProvider": "other"})))
        .await?;
    let error = server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert_eq!(error.error.code, -32600);
    assert!(
        other
            .received_requests()
            .await
            .expect("request recording enabled")
            .is_empty()
    );

    let updated_providers = if required_selection == "gateway" {
        providers.replace(&gateway.uri(), &other.uri())
    } else {
        providers.clone()
    };
    let updated = format!("model_provider = '{required_selection}'\n{updated_providers}");
    std::fs::write(&requirements_path, &updated)?;
    let updated = list_models(&mut server).await?;
    // Each request reads current requirements, so a newly required provider applies without a restart.
    assert_eq!(
        updated
            .data
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["other-model"]
    );
    let requests_before = [
        gateway
            .received_requests()
            .await
            .expect("recorded provider requests")
            .len(),
        other
            .received_requests()
            .await
            .expect("recorded provider requests")
            .len(),
    ];

    // A warm catalog must not hide a policy load failure or make any stale request.
    std::fs::write(&requirements_path, "model_provider = []")?;
    let id = server
        .send_raw_request("model/list", Some(json!({})))
        .await?;
    let error = server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert_eq!(error.error.code, -32600);
    assert!(error.error.message.contains("failed to load configuration"));
    assert_eq!(
        [
            gateway
                .received_requests()
                .await
                .expect("recorded provider requests")
                .len(),
            other
                .received_requests()
                .await
                .expect("recorded provider requests")
                .len(),
        ],
        requests_before,
    );
    assert_eq!(
        other
            .received_requests()
            .await
            .expect("request recording enabled")
            .len(),
        1
    );

    std::fs::remove_file(requirements_path)?;
    assert_eq!(list_models(&mut server).await?, initial);
    // The redefined provider keeps its ID and therefore its cache file, so restoring the definition refetches once.
    assert_eq!(
        gateway
            .received_requests()
            .await
            .expect("request recording enabled")
            .len(),
        if required_selection == "gateway" {
            2
        } else {
            1
        }
    );
    Ok(())
}

/// Startup recovery must not turn malformed current policy into catalog authority.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_list_rejects_invalid_config_across_account_changes() -> Result<()> {
    let home = TempDir::new()?;
    std::fs::write(home.path().join("config.toml"), "model = [")?;
    let mut server = TestAppServer::builder()
        .with_mock_chatgpt_backend()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(Duration::from_secs(/*secs*/ 60))
        .await?;
    for transition in 0..3 {
        if transition == 1 {
            sign_in(&mut server).await?;
        } else if transition == 2 {
            let id = server.send_logout_account_request().await?;
            let _: LogoutAccountResponse = server.read_response(id).await?;
        }
        let id = server
            .send_raw_request("model/list", Some(json!({})))
            .await?;
        let error = server
            .read_stream_until_error_message(RequestId::Integer(id))
            .await?;
        assert_eq!(error.error.code, -32600);
        assert!(error.error.message.contains("failed to load configuration"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_list_selects_a_provider_without_replacing_the_startup_catalog() -> Result<()> {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    // Startup refresh and model/list can fetch concurrently from the default provider.
    for (server, slug) in [(&first, "first-model"), (&second, "second-model")] {
        Mock::given(method("GET"))
            .and(path_regex(".*/models$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalog(slug)?))
            .mount(server)
            .await;
    }
    let home = TempDir::new()?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"
model_provider = "first"
[model_providers.first]
name = "First"
base_url = "{}/v1"
requires_openai_auth = true
[model_providers.second]
name = "Second"
base_url = "{}/v1"
requires_openai_auth = true
"#,
            first.uri(),
            second.uri(),
        ),
    )?;
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("chatgpt-access-token").plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized()
        .await?;
    let initial = list_models(&mut server).await?;
    assert_eq!(initial.data.len(), 1);
    assert_eq!(initial.data[0].id, "first-model");
    let cache_path = home.path().join("models_cache_first.json");
    let initial_cache: ModelsResponse = serde_json::from_slice(&std::fs::read(&cache_path)?)?;

    let select_second = |request_id| ClientRequest::ModelList {
        request_id,
        params: ModelListParams {
            model_provider: Some("second".into()),
            ..Default::default()
        },
    };
    let selected: ModelListResponse = server.request(select_second).await?;
    assert_eq!(selected.data.len(), 1);
    assert_eq!(selected.data[0].id, "second-model");
    let disk_catalog: ModelsResponse = serde_json::from_slice(&std::fs::read(&cache_path)?)?;
    assert_eq!(disk_catalog, initial_cache);
    assert_eq!(list_models(&mut server).await?, initial);
    Ok(())
}

#[tokio::test]
async fn model_list_reports_an_unconfigured_provider() -> Result<()> {
    let mut server = TestAppServer::builder().build_initialized().await?;
    let id = server
        .send_raw_request("model/list", Some(json!({"modelProvider": "missing"})))
        .await?;
    let error = server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.data,
        Some(json!({"type": "modelProviderNotConfigured", "modelProvider": "missing"}))
    );
    Ok(())
}

#[tokio::test]
async fn model_list_follows_active_provider_auth_edit() -> Result<()> {
    use core_test_support::mock_server;
    let gateway = MockServer::start().await;
    mock_server::run(&[&gateway], async {
    for (token, model) in [("initial-key", "first-model"), ("saved-key", "saved-model")] {
        Mock::given(header("authorization", format!("Bearer {token}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalog(model)?))
            .mount(&gateway)
            .await;
    }
    let home = TempDir::new()?;
    let path = home.path().join("config.toml");
    let config = format!(
        r#"model_provider = 'gateway'
[features]
api_key_model_discovery = true
[model_providers.gateway]
name = 'Gateway'
base_url = '{0}'
model_catalog_url = '{0}/models'
env_key = 'GATEWAY_KEY'
[model_providers.other]
name = 'Other'
"#,
        gateway.uri(),
    );
    std::fs::write(&path, &config)?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_managed_config()
        .with_env_overrides(&[
            ("GATEWAY_KEY", Some("initial-key")),
            ("NEW_GATEWAY_KEY", Some("saved-key")),
            ("OPENAI_API_KEY", None),
            ("CODEX_API_KEY", None),
            ("CODEX_ACCESS_TOKEN", None),
            ("OPENAI_FEDERATION_RULE_ID", None),
            ("OPENAI_IDENTITY_TOKEN_FILE", None),
            ("OPENAI_WORKLOAD_IDENTITY_CONTEXT", None),
        ])
        .build_initialized()
        .await?;
    let initial = list_models(&mut server).await?;
    assert_eq!(initial.data[0].id, "first-model");

    // Global lists reload the current route; removing its key cannot reuse old credentials.
    std::fs::write(&path, config.replace("env_key = 'GATEWAY_KEY'", ""))?;
    let id = server
        .send_request(
            "modelProvider/config/write",
            Some(json!({
                "settings": {"type":"customGateway", "providerId": "other", "name": "Renamed"}
            })),
        )
        .await?;
    let _: ModelProviderConfigWriteResponse = server.read_response(id).await?;
    let id = server
        .send_request(
            "modelProvider/config/write",
            Some(json!({
                "settings": {"type":"customGateway", "providerId": "gateway", "baseUrl": "invalid"}
            })),
        )
        .await?;
    server
        .read_stream_until_error_message(RequestId::Integer(id))
        .await?;
    let unavailable = list_models(&mut server).await?;
    assert!(unavailable.data.is_empty());
    assert_eq!(unavailable.model_provider.as_deref(), Some("gateway"));

    let id = server
        .send_request(
            "modelProvider/config/write",
            Some(json!({"settings": {"type":"customGateway", "providerId": "gateway", "envKey": "NEW_GATEWAY_KEY"}})),
        )
        .await?;
    let _: ModelProviderConfigWriteResponse = server.read_response(id).await?;
    assert_eq!(list_models(&mut server).await?.data[0].id, "saved-model");
    Ok(())
    })
    .await
}
