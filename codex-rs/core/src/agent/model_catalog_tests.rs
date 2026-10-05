use super::*;
use crate::agent::child_config::SpawnConfigOptions;
use crate::agent::child_config::SpawnConfigVersion;
use crate::agent::child_config::SpawnModelPrecedence;
use crate::agent::child_config::prepare_agent_spawn_config;
use crate::agent::types::SpawnAgentForkMode;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context_with_auth_and_config_and_rx;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::openai_models::ModelsResponse;
use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[test_case::test_case("openai", "claude", "claude-test-child"; "openai_to_claude")]
#[test_case::test_case("claude", "openai", "gpt-test-child"; "claude_to_openai")]
#[tokio::test]
async fn fresh_child_routes_model_and_provider_but_history_forks_stay_locked(
    parent_id: &str,
    child_id: &str,
    child_model: &str,
) {
    // A parent's catalog cannot validate another provider's models, even though
    // fresh-child execution can use that provider. Both directions must agree.
    let server = MockServer::start().await;
    let mut model = codex_models_manager::bundled_models_response()
        .unwrap()
        .models
        .remove(0);
    model.supported_in_api = true;
    model.multi_agent_version = Some(MultiAgentVersion::V2);
    model.context_window = Some(300_000);
    model.max_context_window = Some(500_000);
    for (slug, endpoint) in [
        ("gpt-test-child", "/models"),
        ("claude-test-child", "/v1/codex/claude/models"),
    ] {
        let mut model = model.clone();
        model.slug = slug.into();
        model.display_name = slug.into();
        Mock::given(method("GET"))
            .and(path(endpoint))
            .respond_with(
                ResponseTemplate::new(/*s*/ 200).set_body_json(ModelsResponse {
                    models: vec![model],
                }),
            )
            .expect(1..)
            .mount(&server)
            .await;
    }
    let provider = |id: &str| {
        if id == "claude" {
            // The owned adapter discovers models without a custom catalog URL.
            ModelProviderInfo {
                adapter: ModelProviderAdapter::ClaudeSubscription,
                name: id.into(),
                base_url: Some(format!("{}/v1", server.uri())),
                ..Default::default()
            }
        } else {
            ModelProviderInfo {
                name: id.into(),
                model_catalog_url: Some(format!("{}/models", server.uri()).into()),
                experimental_bearer_token: Some("test-bearer".into()),
                ..Default::default()
            }
        }
    };
    let parent_provider = provider(parent_id);
    let child_provider = provider(child_id);
    let (session, turn, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("test-key"),
        Vec::new(),
        |config| {
            config.model_provider_id = parent_id.into();
            config.model_provider = parent_provider.clone();
            config.model_provider_context_defaults = [
                (
                    "openai".into(),
                    codex_config::config_toml::ModelProviderContextDefaults {
                        model_context_window: Some(400_000),
                        model_auto_compact_token_limit: Some(300_000),
                    },
                ),
                (
                    "claude".into(),
                    codex_config::config_toml::ModelProviderContextDefaults {
                        model_context_window: Some(1_000_000),
                        model_auto_compact_token_limit: Some(750_000),
                    },
                ),
            ]
            .into();
            config.model_providers = [
                (parent_id.into(), parent_provider),
                (child_id.into(), child_provider.clone()),
            ]
            .into();
            config.features.enable(Feature::MultiAgentV2).unwrap();
            config
                .features
                .enable(Feature::ApiKeyModelDiscovery)
                .unwrap();
        },
    )
    .await;
    session
        .services
        .models_manager
        .set_api_key_model_discovery_enabled(true);
    let step = StepContext::for_test(turn.clone());
    let mut catalog = session
        .agent_model_catalogs(&turn.config, RefreshStrategy::OnlineIfUncached)
        .await;
    let advertised = advertised_agent_models(&catalog, MultiAgentVersion::V2);
    for selector in ["gpt-test-child", "claude-test-child"] {
        assert!(advertised.iter().any(|model| model.model == selector));
    }
    for requested in [child_model.to_owned(), format!("{child_id}/{child_model}")] {
        let prepared = prepare_agent_spawn_config(
            &session,
            &step,
            SpawnConfigOptions {
                version: SpawnConfigVersion::V2,
                model_precedence: SpawnModelPrecedence::RoleOverrides,
                fork_mode: None,
                role_name: None,
                model: Some(&requested),
                reasoning_effort: None,
                service_tier: None,
            },
        )
        .await
        .unwrap();
        // Defaults must follow the child's provider, not the parent's resolved
        // window. Both the advertised maximum and compaction headroom still apply.
        let manager = session
            .agent_models_manager(&prepared.config)
            .await
            .unwrap();
        let child_info = manager
            .get_model_info(child_model, &prepared.config.to_models_manager_config())
            .await;
        let expected = if child_id == "openai" {
            (Some(400_000), Some(300_000))
        } else {
            (Some(500_000), Some(450_000))
        };
        assert_eq!(
            (
                child_info.resolved_context_window(),
                child_info.auto_compact_token_limit()
            ),
            expected,
        );
        assert_eq!(
            (
                prepared.config.model_provider_id,
                prepared.config.model_provider,
                prepared.config.model
            ),
            (
                child_id.into(),
                child_provider.clone(),
                Some(child_model.into())
            ),
        );
        for fork_mode in [
            SpawnAgentForkMode::LastNTurns(1),
            SpawnAgentForkMode::FullHistory,
        ] {
            let error = prepare_agent_spawn_config(
                &session,
                &step,
                SpawnConfigOptions {
                    version: SpawnConfigVersion::V2,
                    model_precedence: SpawnModelPrecedence::RoleOverrides,
                    fork_mode: Some(&fork_mode),
                    role_name: None,
                    model: Some(&requested),
                    reasoning_effort: None,
                    service_tier: None,
                },
            )
            .await
            .err()
            .expect("cross-provider history replay must fail");
            assert!(error.contains("retain the parent model provider"));
        }
    }

    // Even a provider's hidden alternative must keep a colliding slug qualified.
    let mut collision = catalog[0].models[0].clone();
    collision.show_in_picker = false;
    let qualified = format!("{}/{}", catalog[0].provider_id, collision.model);
    let unique = catalog[1].models[0].model.clone();
    catalog[1].models.push(collision);
    assert_eq!(
        advertised_agent_models(&catalog, MultiAgentVersion::V2)
            .into_iter()
            .map(|model| model.model)
            .collect::<Vec<_>>(),
        vec![qualified, unique],
    );
}

#[tokio::test]
async fn claude_parent_secondary_catalog_rebinds_chatgpt_accounts_and_recovers() {
    use crate::agent::control::ProviderAccountSelection;
    use crate::session::tests::make_session_and_context;
    use base64::Engine;
    use codex_login::AuthCredentialsStoreMode;
    use codex_login::AuthKeyringBackendKind;
    use codex_login::AuthManager;
    use codex_login::auth::save_auth;
    use serde_json::json;
    use wiremock::matchers::header;

    let home = tempfile::tempdir().unwrap();
    let root = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("unused-ambient-key"),
        home.path().to_owned(),
    );
    for id in ["alice", "bob"] {
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "email": format!("{id}@example.com"),
                "https://api.openai.com/auth": {
                    "chatgpt_user_id": id, "chatgpt_account_id": id, "chatgpt_plan_type": "pro"
                }
            }))
            .unwrap(),
        );
        save_auth(home.path(), &serde_json::from_value(json!({
            "auth_mode": "chatgpt",
            "last_refresh": chrono::Utc::now().to_rfc3339(),
            "tokens": {"id_token": format!("e30.{claims}.c2ln"), "access_token": format!("access-{id}"),
                "refresh_token": format!("refresh-{id}"), "account_id": id}
        })).unwrap(), AuthCredentialsStoreMode::File, AuthKeyringBackendKind::default()).unwrap();
    }
    let accounts = root.saved_accounts().unwrap();
    let alice = accounts
        .iter()
        .find(|account| account.email.as_deref() == Some("alice@example.com"))
        .unwrap()
        .id
        .clone();
    let bob = accounts
        .iter()
        .find(|account| account.email.as_deref() == Some("bob@example.com"))
        .unwrap()
        .id
        .clone();
    let server = MockServer::start().await;
    for id in ["alice", "bob"] {
        let response = if id == "alice" {
            ResponseTemplate::new(/*s*/ 200).set_body_json(ModelsResponse::default())
        } else {
            ResponseTemplate::new(/*s*/ 500)
        };
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", format!("Bearer access-{id}")))
            .and(header("chatgpt-account-id", id))
            .respond_with(response)
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
    }
    let (mut session, turn) = make_session_and_context().await;
    session.provider_id = "claude".into();
    session.services.auth_manager = root.for_account(&alice).await.unwrap();
    let mut config = (*turn.config).clone();
    config.codex_home =
        codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path_checked(home.path())
            .unwrap();
    config.model_provider_id = "openai".into();
    config.model_provider = ModelProviderInfo {
        name: "OpenAI".into(),
        requires_openai_auth: true,
        request_max_retries: Some(0),
        model_catalog_url: Some(format!("{}/models", server.uri()).into()),
        ..Default::default()
    };
    config.wrangler.default_account = Some(alice.clone());
    // A Claude root retains an incidental ChatGPT manager and a startup config, neither
    // of which may override a newer /account choice for fresh OpenAI children.
    tokio::fs::write(
        config.codex_home.join("config.toml"),
        format!("[wrangler]\ndefault_account = {bob:?}\n"),
    )
    .await
    .unwrap();
    let child_auth = account_auth_for_provider(
        &config,
        &session.services.local_agent_runtime.account_selection,
        &session.services.auth_manager,
        /*persisted*/ None,
        &AccountInheritance::OtherProvider,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(child_auth.selected_account_id(), Some(bob.clone()));
    let inherited_auth = account_auth_for_provider(
        &config,
        &session.services.local_agent_runtime.account_selection,
        &session.services.auth_manager,
        /*persisted*/ None,
        &AccountInheritance::SameProvider(session.services.auth_manager.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(inherited_auth.selected_account_id(), Some(alice.clone()));
    let new_root_auth = account_auth_for_provider(
        &config,
        &session.services.local_agent_runtime.account_selection,
        &session.services.auth_manager,
        /*persisted*/ None,
        &AccountInheritance::None,
    )
    .await
    .unwrap()
    .unwrap();
    let resumed_auth = account_auth_for_provider(
        &config,
        &session.services.local_agent_runtime.account_selection,
        &session.services.auth_manager,
        Some(alice.clone()),
        &AccountInheritance::OtherProvider,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        (
            new_root_auth.selected_account_id(),
            resumed_auth.selected_account_id()
        ),
        (Some(alice.clone()), Some(alice.clone()))
    );
    let first = session.agent_models_manager(&config).await.unwrap();
    first
        .list_models(
            RefreshStrategy::OnlineIfUncached,
            config.http_client_factory(),
        )
        .await;
    let retained = session.agent_models_manager(&config).await.unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &retained));
    tokio::fs::write(
        config.codex_home.join("config.toml"),
        format!("[wrangler]\ndefault_account = {alice:?}\n"),
    )
    .await
    .unwrap();
    let rebound = session.agent_models_manager(&config).await.unwrap();
    rebound
        .list_models(
            RefreshStrategy::OnlineIfUncached,
            config.http_client_factory(),
        )
        .await;
    assert!(!std::sync::Arc::ptr_eq(&first, &rebound));
    session
        .services
        .local_agent_runtime
        .account_selection
        .select("openai", ProviderAccountSelection::Chatgpt { id: alice });
    tokio::fs::write(
        config.codex_home.join("config.toml"),
        format!("[wrangler]\ndefault_account = {bob:?}\n"),
    )
    .await
    .unwrap();
    let bound = session.agent_models_manager(&config).await.unwrap();
    assert!(std::sync::Arc::ptr_eq(&rebound, &bound));
    // The failed Bob discovery did not run again during metadata lookup; /account selected Alice's actual
    // bearer and account header instead of reusing Bob or the ambient/default login.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    server.verify().await;
    server.reset().await;
    let mut recovered_model = codex_models_manager::bundled_models_response()
        .unwrap()
        .models
        .remove(0);
    recovered_model.slug = "gpt-test-recovered".into();
    recovered_model.supported_in_api = true;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", "Bearer access-bob"))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200).set_body_json(ModelsResponse {
                models: vec![recovered_model],
            }),
        )
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let recovered = first
        .list_models(
            RefreshStrategy::OnlineIfUncached,
            config.http_client_factory(),
        )
        .await;
    assert!(
        recovered
            .iter()
            .any(|model| model.model == "gpt-test-recovered")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
