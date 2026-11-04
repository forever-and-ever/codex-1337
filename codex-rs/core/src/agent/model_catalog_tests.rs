use super::*;
use crate::agent::child_config::SpawnConfigOptions;
use crate::agent::child_config::SpawnConfigVersion;
use crate::agent::child_config::SpawnModelPrecedence;
use crate::agent::child_config::prepare_agent_spawn_config;
use crate::agent::types::SpawnAgentForkMode;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context_with_auth_config_home_and_rx;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_login::anthropic::AccountIdentity;
use codex_login::anthropic::AccountInfo;
use codex_login::anthropic::AccountStore;
use codex_login::anthropic::SubscriptionGrant;
use codex_model_provider_info::ANTHROPIC_BASE_URL;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::openai_models::AnthropicModelCapabilities;
use codex_protocol::openai_models::AnthropicThinkingMode;
use codex_protocol::openai_models::ModelsResponse;
use pretty_assertions::assert_eq;
use std::sync::Arc;
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
    let home = tempfile::tempdir().unwrap();
    let stalled_catalog = Arc::new(tokio::sync::Notify::new());
    let account_id = AccountStore::new(home.path().to_owned())
        .upsert(
            AccountInfo {
                identity: AccountIdentity::new("claude-test-child").unwrap(),
                email: None,
                organization_uuid: None,
                organization_name: None,
            },
            SubscriptionGrant::bearer_only("test-bearer".into()).unwrap(),
        )
        .await
        .unwrap()
        .to_string();
    let mut model = codex_models_manager::bundled_models_response()
        .unwrap()
        .models
        .remove(0);
    model.supported_in_api = true;
    model.multi_agent_version = Some(MultiAgentVersion::V2);
    model.context_window = Some(300_000);
    model.max_context_window = Some(500_000);
    {
        let mut openai = model.clone();
        openai.slug = "gpt-test-child".into();
        openai.display_name = openai.slug.clone();
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(
                ResponseTemplate::new(/*s*/ 200).set_body_json(ModelsResponse {
                    models: (0..2)
                        .map(|index| {
                            let mut extra = openai.clone();
                            extra.slug = format!("gpt-extra-{index}");
                            extra.display_name = extra.slug.clone();
                            extra
                        })
                        .chain(std::iter::once(openai.clone()))
                        .collect(),
                }),
            )
            .expect(1..)
            .mount(&server)
            .await;
    }
    let provider = |id: &str| {
        if id == "claude" {
            ModelProviderInfo {
                adapter: ModelProviderAdapter::ClaudeSubscription,
                name: id.into(),
                base_url: Some(ANTHROPIC_BASE_URL.into()),
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
    model.slug = "claude-test-child".into();
    model.display_name = model.slug.clone();
    model.anthropic = Some(AnthropicModelCapabilities {
        thinking: AnthropicThinkingMode::Disabled,
        max_output_tokens: std::num::NonZeroU32::new(/*n*/ 8192).unwrap(),
        supported_efforts: Vec::new(),
        supports_structured_outputs: false,
    });
    let native_catalog = ModelsResponse {
        models: (0..2)
            .map(|index| {
                let mut extra = model.clone();
                extra.slug = format!("claude-extra-{index}");
                extra.display_name = extra.slug.clone();
                extra
            })
            .chain(std::iter::once(model.clone()))
            .collect(),
    };
    let (session, turn, _rx) = Box::pin(make_session_and_context_with_auth_config_home_and_rx(
        CodexAuth::from_api_key("test-key"),
        Vec::new(),
        home.path(),
        |config| {
            config.wrangler.default_claude_account = Some(account_id.clone());
            config.model_provider_id = parent_id.into();
            config.model_provider = parent_provider.clone();
            if parent_id == "claude" {
                config.model_catalog = Some(native_catalog.clone());
            }
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
    ))
    .await;
    if parent_id == "claude" {
        session.services.model_client.commit_provider_account(
            ProviderAccountSelection::ClaudeSubscription { id: account_id },
        );
    } else {
        let mut native_config = (*turn.config).clone();
        native_config.model_provider_id = "claude".into();
        native_config.model_provider = provider("claude");
        native_config.model_catalog = None;
        session.agent_models_manager(&native_config).await.unwrap();
        // Exercise cold discovery without sending subscription credentials off-provider.
        // The explicit endpoint has the same empty fallback as the native Claude endpoint.
        Mock::given(method("GET"))
            .and(path("/claude_models"))
            .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(&native_catalog))
            .expect(1..)
            .mount(&server)
            .await;
        let started = Arc::clone(&stalled_catalog);
        Mock::given(method("GET"))
            .and(path("/claude_models"))
            .respond_with(move |_: &wiremock::Request| {
                started.notify_one();
                ResponseTemplate::new(/*s*/ 200)
                    .set_body_json(&native_catalog)
                    .set_delay(CATALOG_TIMEOUT + Duration::from_secs(/*secs*/ 1))
            })
            .up_to_n_times(/*n*/ 1)
            .with_priority(/*p*/ 1)
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
        let manager = codex_model_provider::create_model_provider(
            ModelProviderInfo {
                name: "Cold Claude catalog fixture".into(),
                model_catalog_url: Some(format!("{}/claude_models", server.uri()).into()),
                experimental_bearer_token: Some("test-bearer".into()),
                ..Default::default()
            },
            None,
        )
        .models_manager_without_cache(None);
        manager.set_api_key_model_discovery_enabled(true);
        assert_eq!(manager.try_list_models().unwrap(), []);
        session
            .services
            .agent_models_managers
            .cached
            .lock()
            .await
            .get_mut("claude")
            .unwrap()
            .manager = manager;
    }
    session
        .services
        .models_manager
        .set_api_key_model_discovery_enabled(true);
    let step = StepContext::for_test(turn.clone());
    // Ordinary turns must discover an empty catalog, then retry after a timed-out
    // first fetch instead of keeping a cross-provider manager empty for the session.
    let first = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 2),
        session.agent_model_catalogs(&turn.config, RefreshStrategy::Offline),
    )
    .await
    .expect("ordinary turns must not await the stalled catalog");
    if parent_id == "openai" {
        tokio::time::timeout(Duration::from_secs(/*secs*/ 2), stalled_catalog.notified())
            .await
            .unwrap();
    }
    // A second offline read must neither wait behind the refresh semaphore nor
    // start a duplicate refresh while the first request is still stalled.
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 2),
        session.agent_model_catalogs(&turn.config, RefreshStrategy::Offline),
    )
    .await
    .expect("ordinary turns must not join an ongoing network refresh");
    let pending = std::mem::take(&mut *session.services.agent_models_managers.warming.lock().await);
    for (id, mut warm) in pending {
        if !warm.task.is_finished() {
            (&mut warm.task).await.unwrap();
        }
        session
            .services
            .agent_models_managers
            .warming
            .lock()
            .await
            .insert(id, warm);
    }
    if parent_id == "openai" {
        assert!(
            !advertised_agent_models(&first, MultiAgentVersion::V2)
                .iter()
                .any(|model| model.model.starts_with("claude-"))
        );
    }
    session
        .agent_model_catalogs(&turn.config, RefreshStrategy::Offline)
        .await;
    let pending = std::mem::take(&mut *session.services.agent_models_managers.warming.lock().await);
    for (id, mut warm) in pending {
        if !warm.task.is_finished() {
            (&mut warm.task).await.unwrap();
        }
        session
            .services
            .agent_models_managers
            .warming
            .lock()
            .await
            .insert(id, warm);
    }
    let mut catalog = session
        .agent_model_catalogs(&turn.config, RefreshStrategy::Offline)
        .await;
    let warm = session
        .agent_model_catalogs(&turn.config, RefreshStrategy::Offline)
        .await;
    assert_eq!(
        advertised_agent_models(&catalog, MultiAgentVersion::V2),
        advertised_agent_models(&warm, MultiAgentVersion::V2),
    );
    if parent_id == "openai" {
        assert_eq!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.url.path() == "/claude_models")
                .count(),
            2,
        );
    }
    let advertised = advertised_agent_models(&catalog, MultiAgentVersion::V2);
    // The cross-provider child is sixth in either direction, beyond the old
    // advertising cutoff, even though this same catalog accepts its selector.
    assert_eq!(advertised.len(), 6);
    for selector in ["gpt-test-child", "claude-test-child"] {
        assert!(advertised.iter().any(|model| model.model == selector));
    }
    for requested in [child_model.to_owned(), format!("{child_id}/{child_model}")] {
        let prepared = Box::pin(prepare_agent_spawn_config(
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
        ))
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
        {
            let fork_mode = SpawnAgentForkMode::FullHistory;
            let error = Box::pin(prepare_agent_spawn_config(
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
            ))
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
    let selectors = advertised_agent_models(&catalog, MultiAgentVersion::V2)
        .into_iter()
        .map(|model| model.model)
        .collect::<Vec<_>>();
    assert_eq!(selectors.len(), 6);
    assert!(selectors.contains(&qualified));
    assert!(selectors.contains(&unique));
    assert!(!selectors.contains(&catalog[0].models[0].model));
}

#[tokio::test]
async fn claude_parent_secondary_catalog_rebinds_chatgpt_accounts_and_recovers() {
    use crate::agent::control::ProviderAccountSelection;
    use crate::session::tests::make_session_and_context;
    use base64::Engine;
    use codex_login::AuthCredentialsStoreMode;
    use codex_login::AuthKeyringBackendKind;
    use codex_login::AuthManager;
    use codex_login::auth::activate_account;
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
    let (mut session, turn) = Box::pin(make_session_and_context()).await;
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
    let activate = |id: &str| {
        activate_account(
            home.path(),
            id,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .unwrap();
    };
    // A Claude root retains an incidental ChatGPT manager, which may not override a newer
    // /account choice for fresh OpenAI children.
    activate(&bob);
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
        (Some(bob.clone()), Some(alice.clone()))
    );
    let first = session.agent_models_manager(&config).await.unwrap();
    first
        .list_models(
            RefreshStrategy::OnlineIfUncached,
            config.http_client_factory(),
        )
        .await;
    let retained = session.agent_models_manager(&config).await.unwrap();
    assert!(Arc::ptr_eq(&first, &retained));
    activate(&alice);
    let rebound = session.agent_models_manager(&config).await.unwrap();
    rebound
        .list_models(
            RefreshStrategy::OnlineIfUncached,
            config.http_client_factory(),
        )
        .await;
    assert!(!Arc::ptr_eq(&first, &rebound));
    session
        .services
        .local_agent_runtime
        .account_selection
        .select("openai", ProviderAccountSelection::Chatgpt { id: alice });
    activate(&bob);
    let bound = session.agent_models_manager(&config).await.unwrap();
    assert!(Arc::ptr_eq(&rebound, &bound));
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

#[test]
fn selectors_respect_provider_prefixes_before_openrouter_slugs() {
    let manager = codex_model_provider::create_model_provider(ModelProviderInfo::default(), None)
        .models_manager_without_cache(None);
    let base: ModelPreset = codex_models_manager::bundled_models_response()
        .unwrap()
        .models
        .remove(0)
        .into();
    let catalog = |provider: &str, slugs: &[&str]| ProviderModels {
        provider_id: provider.into(),
        manager: manager.clone(),
        models: slugs
            .iter()
            .map(|slug| {
                let mut model = base.clone();
                model.model = (*slug).into();
                model.show_in_picker = true;
                model.multi_agent_version = None;
                model
            })
            .collect(),
    };
    let catalogs = [
        catalog("openai", &["gpt-5.5"]),
        catalog("openrouter", &["moonshotai/kimi-k3", "openai/gpt-5.5"]),
    ];
    // A slash can belong to a provider selector or to the saved model itself.
    for (selector, provider, slug) in [
        (
            "openrouter/moonshotai/kimi-k3",
            "openrouter",
            "moonshotai/kimi-k3",
        ),
        ("moonshotai/kimi-k3", "openrouter", "moonshotai/kimi-k3"),
        ("openai/gpt-5.5", "openai", "gpt-5.5"),
        ("openrouter/openai/gpt-5.5", "openrouter", "openai/gpt-5.5"),
    ] {
        let (resolved, model) = crate::agent::child_config::find_spawn_agent_model(
            &catalogs,
            selector,
            MultiAgentVersion::V2,
            |id| catalogs.iter().any(|catalog| catalog.provider_id == id),
        )
        .unwrap();
        assert_eq!((resolved.provider_id.as_str(), model), (provider, slug));
    }
    let advertised = advertised_agent_models(&catalogs, MultiAgentVersion::V2);
    assert!(
        advertised
            .iter()
            .any(|model| model.model == "openrouter/openai/gpt-5.5")
    );
}
