//! Exercise selected transport revocation with runtime auth enabled.
#![cfg(test)]

use super::*;
use crate::client::ClientRouting;
use crate::client::RequestRouteTelemetry;
use codex_client::HttpTransport;
use codex_http_client::DestinationPolicy;
use codex_http_client::NetworkPolicyController;
use codex_http_client::NetworkPolicyDenied;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthRouteConfig;
use pretty_assertions::assert_eq;

#[test_case::test_case(RuntimeAuthMode::DualHeader; "dual_header")]
#[test_case::test_case(RuntimeAuthMode::AssertionOnly; "assertion_only")]
#[tokio::test]
async fn selected_runtime_transport_stays_revoked(mode: RuntimeAuthMode) {
    let server = MockServer::start().await;
    // This custom inference origin must never receive runtime identity. The unfiltered
    // credential binding still retains the actor for separately admitted native services.
    Mock::given(path("/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "runtime_agent_instance_id": "runtime-a", "agent_host_id": "host-a",
            "actor": {"kind": "platform", "user_id": "user-a", "organization_id": "org-a", "project_id": null},
        })))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let controller = NetworkPolicyController::default();
    let policy = controller.policy();
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
        .with_network_policy(policy.clone());
    let home = TempDir::new().unwrap();
    codex_login::login_with_api_key(
        home.path(),
        "key-a",
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::Direct,
    )
    .unwrap();
    let manager = Arc::new(
        AuthManager::new(
            home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::Ephemeral,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::Direct,
            AuthRouteConfig::from_http_client_factory(factory.clone()),
        )
        .await,
    );
    assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
    let runtime_source = Arc::new(ChangingPrincipalSource(std::sync::RwLock::new(
        CodexAuth::from_api_key("key-a"),
    )));
    manager
        .set_external_auth(runtime_source.clone())
        .await
        .unwrap();
    assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
    let parent_permit = policy
        .acquire(&format!("{}/v1/responses", server.uri()).parse().unwrap())
        .unwrap();
    let runtime = Arc::new(RuntimeAuthManager::new(
        manager.clone(),
        AgentHostAuthorization::InstallationId {
            installation_id: uuid::Uuid::nil(),
        },
        RuntimeRegistrationMetadata {
            harness: None,
            harness_version: None,
            role: RuntimeRole::OrchestratorAndExecutor,
        },
        format!("{}/register", server.uri()).parse().unwrap(),
    ));
    let mut client = test_model_client(SessionSource::Exec);
    client.http_client_factory = factory;
    Arc::get_mut(&mut client.state).unwrap().provider = create_model_provider(
        ModelProviderInfo::create_openai_provider(Some(format!("{}/v1", server.uri()))),
        Some(Arc::clone(&manager)),
    );
    let client = client
        .with_runtime_auth(Arc::clone(&runtime), mode)
        .unwrap();
    let captured = client.capture_provider().unwrap();
    assert!(captured.same_authority(&client.capture_provider().unwrap()));
    assert!(server.received_requests().await.unwrap().is_empty());
    parent_permit.check().unwrap();
    let mut auxiliary = test_model_client(SessionSource::Exec);
    Arc::get_mut(&mut auxiliary.state).unwrap().provider =
        create_model_provider(client.state.provider.info().clone(), None);
    let auxiliary = auxiliary.with_captured_provider(captured).unwrap();
    let selected = auxiliary.state.provider.credential_runtime().unwrap();
    assert!(selected.0.same_registration_authority(&runtime));
    assert!(auxiliary.auth_manager().unwrap().has_external_auth());
    let mut setup = client
        .current_client_setup(ClientRouting::Workspace)
        .await
        .unwrap();
    setup.api_provider.base_url = format!("{}/v1", server.uri());
    let transport = setup.http_transport("/responses").unwrap();
    let request = setup
        .api_provider
        .build_request(http::Method::POST, "/responses");
    let request = setup.api_auth.apply_auth(request).await.unwrap();
    transport.execute(request.clone()).await.unwrap();
    *runtime_source.0.write().unwrap() = CodexAuth::from_api_key("rotated-key-a");
    let rotated = auxiliary
        .current_client_setup(ClientRouting::Workspace)
        .await
        .unwrap();
    assert_eq!(rotated.auth.unwrap().get_token().unwrap(), "rotated-key-a");
    parent_permit.check().unwrap();

    // A new policy may allow the destination, but cannot authorize the old account's request.
    policy.invalidate();
    assert_eq!(parent_permit.check(), Err(NetworkPolicyDenied::Revoked));
    assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
    let revoked = transport.execute(request).await;
    assert!(
        matches!(
            revoked,
            Err(TransportError::Policy(NetworkPolicyDenied::Revoked))
        ),
        "old transport escaped its revocation fence: {revoked:?}"
    );
    let auth_context = AuthRequestTelemetryContext::new(
        setup.auth.as_ref().map(CodexAuth::auth_mode),
        setup.api_auth.as_ref(),
        /*agent_identity_telemetry*/ None,
        PendingUnauthorizedRetry::default(),
    )
    .unwrap();
    let metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        "test-session".into(),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::WebsocketConnection,
    );
    let result = client
        .connect_websocket(
            &test_session_telemetry(),
            setup.api_provider,
            setup.api_auth,
            &setup.http_client_factory,
            &metadata,
            auth_context,
            RequestRouteTelemetry::for_endpoint("/responses"),
            &http::HeaderMap::new(),
            /*parent_routing_affinity*/ None,
            /*reauth*/ false,
        )
        .await;
    assert!(matches!(
        result,
        Err(ApiError::Transport(TransportError::Policy(
            NetworkPolicyDenied::Revoked
        )))
    ));
    // Replacing this runtime's credential principal revokes captured work without
    // authorizing a second registration or an anonymous/provider fallback.
    codex_login::login_with_api_key(
        home.path(),
        "key-b",
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::Direct,
    )
    .unwrap();
    manager.clear_external_auth();
    manager.reload().await;
    assert!(
        auxiliary
            .current_client_setup(ClientRouting::Workspace)
            .await
            .is_err()
    );

    // Catalog auth resolves again after capture. A new principal must not reach
    // either discovery or this retained runtime's registration endpoint.
    struct ChangingPrincipalSource(std::sync::RwLock<CodexAuth>);
    impl codex_login::ExternalAuth for ChangingPrincipalSource {
        fn resolve(&self) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
            let auth = self.0.read().unwrap().clone();
            Box::pin(async move { Ok(auth) })
        }
        fn refresh(
            &self,
            _: codex_login::ExternalAuthRefreshContext,
        ) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
            self.resolve()
        }
    }
    use base64::Engine as _;
    let chatgpt = |user: &str, workspace: &str| {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({"https://api.openai.com/auth": {"chatgpt_user_id": user}}).to_string());
        CodexAuth::from_external_chatgpt_tokens(
            &format!("header.{payload}.signature"),
            workspace,
            None,
        )
        .unwrap()
    };
    for runtime_enabled in [false, true] {
        let initial = if runtime_enabled {
            CodexAuth::from_api_key("catalog-a")
        } else {
            chatgpt("user-a", "workspace-a")
        };
        let source = Arc::new(ChangingPrincipalSource(std::sync::RwLock::new(
            initial.clone(),
        )));
        let manager = AuthManager::from_auth_for_testing(initial);
        manager.set_external_auth(source.clone()).await.unwrap();
        let mut info =
            ModelProviderInfo::create_openai_provider(Some(format!("{}/v1", server.uri())));
        info.model_catalog_url = Some(format!("{}/captured-models", server.uri()).into());
        let mut client = test_model_client(SessionSource::Exec);
        Arc::get_mut(&mut client.state).unwrap().provider =
            create_model_provider(info.clone(), Some(Arc::clone(&manager)));
        if runtime_enabled {
            let runtime = Arc::new(RuntimeAuthManager::new(
                manager,
                AgentHostAuthorization::InstallationId {
                    installation_id: uuid::Uuid::nil(),
                },
                RuntimeRegistrationMetadata {
                    harness: None,
                    harness_version: None,
                    role: RuntimeRole::OrchestratorAndExecutor,
                },
                format!("{}/register", server.uri()).parse().unwrap(),
            ));
            client = client.with_runtime_auth(runtime, mode).unwrap();
        }
        let captured = client.capture_provider().unwrap();
        let mut config = crate::config::test_config().await;
        config.model_provider = info;
        config.model_catalog = None;
        let models = captured.models_manager(&config).unwrap();
        models.set_api_key_model_discovery_enabled(true);
        *source.0.write().unwrap() = chatgpt("user-b", "workspace-b");
        let _ = models
            .authoritative_catalog(
                codex_models_manager::manager::RefreshStrategy::OnlineIfUncached,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;
        assert!(captured.model_info(&config, "gpt-5.5").await.is_err());
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "invalid catalog credentials must be rejected before discovery or registration"
        );
    }
    server.verify().await;
}

#[test_case::test_case(RuntimeAuthMode::DualHeader; "dual_header")]
#[test_case::test_case(RuntimeAuthMode::AssertionOnly; "assertion_only")]
#[tokio::test]
async fn request_owner_tracks_the_selected_explicit_key(mode: RuntimeAuthMode) {
    let server = MockServer::start().await;
    Mock::given(path("/register"))
        .respond_with(|request: &wiremock::Request| ResponseTemplate::new(200).set_body_json(json!({
            "runtime_agent_instance_id": request.headers["idempotency-key"].to_str().unwrap(),
            "agent_host_id": "host-a",
            "actor": {"kind": "platform", "user_id": "user-a", "organization_id": "org-a", "project_id": null},
        })))
        .expect(2)
        .mount(&server).await;
    let credentials = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("saved-key"));
    let context = codex_model_provider::ProviderAuthContext {
        runtime_auth: Some((
            Arc::new(RuntimeAuthManager::new(
                credentials.clone(),
                AgentHostAuthorization::InstallationId {
                    installation_id: uuid::Uuid::nil(),
                },
                RuntimeRegistrationMetadata {
                    harness: None,
                    harness_version: None,
                    role: RuntimeRole::Orchestrator,
                },
                format!("{}/register", server.uri()).parse().unwrap(),
            )),
            mode,
        )),
        credentials,
    };
    let mut owners = Vec::new();
    for key in ["key-a", "key-b", "key-a"] {
        let mut info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        info.experimental_bearer_token = Some(key.into());
        let mut client = test_model_client(SessionSource::Exec);
        Arc::get_mut(&mut client.state).unwrap().provider = context.model_provider(info);
        let owner = client.auth_owner();
        let setup = client
            .current_client_setup(ClientRouting::ConfiguredProvider)
            .await
            .unwrap();
        assert!(owner.is_some());
        assert_eq!(setup.auth_owner, owner);
        assert_eq!(setup.auth.unwrap().api_key(), Some(key));
        let mut headers = http::HeaderMap::new();
        setup.api_auth.add_auth_headers(&mut headers).unwrap();
        match mode {
            RuntimeAuthMode::DualHeader => {
                assert_eq!(
                    headers[http::header::AUTHORIZATION],
                    format!("Bearer {key}")
                );
                assert!(headers.contains_key("x-openai-agent-assertion"));
            }
            RuntimeAuthMode::AssertionOnly => {
                assert!(
                    headers[http::header::AUTHORIZATION]
                        .to_str()
                        .unwrap()
                        .starts_with("AgentAssertion ")
                );
                assert!(!headers.contains_key("x-openai-agent-assertion"));
            }
        }
        owners.push(owner);
    }
    assert_ne!(
        owners[0], owners[1],
        "different keys cannot share turn or connection ownership"
    );
    assert_eq!(
        owners[0], owners[2],
        "reselecting a key preserves its owner"
    );
    server.verify().await;
}
