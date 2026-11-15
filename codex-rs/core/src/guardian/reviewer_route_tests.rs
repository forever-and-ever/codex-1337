use super::CapturedReviewer;
use super::ReviewerRoute;
use crate::guardian::ApprovalRequestReasons;
use crate::guardian::GuardianApprovalRequest;
use crate::guardian::GuardianReviewContext;
use crate::guardian::GuardianReviewOutcome;
use crate::guardian::review::run_guardian_review_session_before_deadline;
use crate::session::tests::make_session_and_context;
use crate::session::tests::update_turn_settings_for_test;
use codex_guardian_reviewer::ReviewDeadline;
use codex_http_client::HttpClientFactory;
use codex_http_client::NetworkPolicy;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::AuthRouteConfig;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::StaticModelsManager;
use codex_protocol::models::SandboxPermissions;
use codex_protocol::openai_models::GuardianScope;
use codex_protocol::openai_models::ModelsResponse;
use codex_utils_path_uri::LegacyAppPathString;
use core_test_support::TempDirExt;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::Instant;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn claude_subscription_review_spawns_chatgpt_reviewer_with_saved_account()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    // Exercise the real reviewer pool/spawn without parent inference: native
    // subscription credentials must never be redirected to a mock origin.
    let home = TempDir::new()?;
    std::fs::write(
        home.path().join("auth.json"),
        serde_json::to_vec(&json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "e30.e30.signature",
                "access_token": "chatgpt-review-token",
                "refresh_token": "chatgpt-refresh-token",
                "account_id": "chatgpt-review-account"
            },
            "last_refresh": chrono::Utc::now()
        }))?,
    )?;
    let server = MockServer::start().await;
    let policy = NetworkPolicy::default().restrict_to_endpoints(
        [
            format!("{}/v1/models", server.uri()).parse()?,
            format!("{}/v1/responses", server.uri()).parse()?,
        ]
        .into(),
    );
    let auth = AuthManager::shared(
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
    .await;
    let saved_account = auth.selected_account_id().expect("saved ChatGPT account");
    let (mut session, mut turn) = Box::pin(make_session_and_context()).await;
    let mut config = turn.config.as_ref().clone();
    let mut openai = config.model_provider.clone();
    openai.base_url = Some(format!("{}/v1", server.uri()));
    openai.supports_websockets = false;
    config.model_providers.insert("openai".into(), openai);
    config.model_provider_id = "claude_subscription".into();
    config.model_provider = ModelProviderInfo {
        adapter: ModelProviderAdapter::ClaudeSubscription,
        name: "Claude subscription".into(),
        ..Default::default()
    };
    config.model_providers.insert(
        config.model_provider_id.clone(),
        config.model_provider.clone(),
    );
    config.codex_home = home.abs();
    config.application_network_policy = policy;
    config.model = Some("claude-sonnet-4-6".into());
    session.provider_id = config.model_provider_id.clone();
    session.services.auth_manager = Arc::clone(&auth);
    update_turn_settings_for_test(&mut turn, |settings| {
        Arc::make_mut(&mut settings.model_info).slug = "claude-sonnet-4-6".into();
    });
    session.services.models_manager = Arc::new(StaticModelsManager::new(
        Some(Arc::clone(&auth)),
        ModelsResponse {
            models: vec![turn.model_info().model.as_ref().clone()],
        },
        codex_models_manager::ModelOrigin::Provider,
    ));
    turn.provider = create_model_provider(config.model_provider.clone(), Some(Arc::clone(&auth)));
    turn.auth_manager = Some(auth);
    turn.config = Arc::new(config);
    crate::guardian::test_host::install(&session, &turn.config);

    let mut catalog_model = codex_models_manager::bundled_models_response()?
        .models
        .remove(0);
    catalog_model.slug = "codex-auto-review".into();
    catalog_model.auto_review_model_override = None;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ModelsResponse {
            models: vec![catalog_model],
        }))
        .mount(&server)
        .await;
    let review_log = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("review"),
            ev_assistant_message("assessment", &json!({"outcome":"allow"}).to_string()),
            ev_completed("review"),
        ]),
    )
    .await;
    let context = GuardianReviewContext::from(Arc::new(turn));
    let session = Arc::new(session);
    let route = Box::pin(ReviewerRoute::resolve(&session, &context)).await;
    assert!(matches!(
        &route,
        ReviewerRoute::ChatgptAutoReview { account_id, .. } if account_id == &saved_account
    ));
    let (outcome, _) = Box::pin(run_guardian_review_session_before_deadline(
        session,
        context,
        GuardianApprovalRequest::ExecCommand {
            id: "review-push".into(),
            environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.into(),
            command: vec!["git".into(), "push".into()],
            cwd: home.abs().into(),
            guardian_cwd: LegacyAppPathString::from_abs_path(&home.abs()),
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            additional_permissions: None,
            justification: Some("Push the reviewed change".into()),
            tty: false,
        },
        GuardianScope::Shell,
        ApprovalRequestReasons {
            approval: None,
            retry: None,
        },
        crate::guardian::review::ReviewAttemptOptions {
            authorization: None,
            external_cancel: None,
            deadline: ReviewDeadline::At(Instant::now() + Duration::from_secs(15)),
        },
        &route,
    ))
    .await;
    assert!(
        matches!(outcome, GuardianReviewOutcome::Completed(_)),
        "{outcome:?}"
    );
    let request = review_log.single_request();
    assert_eq!(request.body_json()["model"], "codex-auto-review");
    assert_eq!(
        request.header("authorization"),
        Some("Bearer chatgpt-review-token".into())
    );
    assert_eq!(
        request.header("chatgpt-account-id"),
        Some("chatgpt-review-account".into())
    );
    let requests = server.received_requests().await.expect("captured requests");
    assert!(requests.iter().all(|request| matches!(
        (request.method.as_str(), request.url.path()),
        ("GET", "/v1/models") | ("POST", "/v1/responses")
    )));
    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn route_failure_quarantines_and_falls_back_only_with_remaining_time() {
    use codex_guardian_reviewer::GuardianAssessment;
    use codex_guardian_reviewer::GuardianReviewError;
    use codex_protocol::protocol::CodexErrorInfo;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::GuardianRiskLevel;
    use codex_protocol::protocol::GuardianUserAuthorization;
    use tokio::sync::Mutex;

    // A transport failure is not a verdict. The fallback must preserve the review's
    // route across retries, and an exhausted deadline must not consume its warning.
    for timeout in [Some(Duration::from_mins(1)), Some(Duration::ZERO), None] {
        let (session, turn, rx) =
            Box::pin(crate::session::tests::make_session_and_context_with_rx()).await;
        let context = GuardianReviewContext::from(turn);
        let account_id = "reviewer-account".to_owned();
        let chosen = ReviewerRoute::ChatgptAutoReview {
            account_id: account_id.clone(),
            captured: CapturedReviewer(Arc::new(
                session
                    .services
                    .model_client
                    .capture_provider()
                    .expect("capture test provider"),
            )),
        };
        let route = Mutex::new(chosen.clone());
        let before = Instant::now();
        let deadline = ReviewDeadline::from_timeout(timeout);
        let time_remains = timeout != Some(Duration::ZERO);
        let assessment = GuardianAssessment {
            risk_level: GuardianRiskLevel::Low,
            user_authorization: GuardianUserAuthorization::Unknown,
            outcome: codex_protocol::protocol::GuardianAssessmentOutcome::Allow,
            rationale: "Parent provider assessment".into(),
        };
        let mut calls = Vec::new();
        let outcome = {
            let mut run = |route: ReviewerRoute| {
                calls.push(route.clone());
                let outcome = match route {
                    ReviewerRoute::ChatgptAutoReview { .. } => {
                        GuardianReviewOutcome::Error(GuardianReviewError::Session {
                            message: "original 401".into(),
                            error_info: Some(CodexErrorInfo::Unauthorized),
                            retry_at: None,
                        })
                    }
                    ReviewerRoute::OpenRouterUnavailable => {
                        unreachable!("Claude test cannot require an OpenRouter login")
                    }
                    ReviewerRoute::ParentProvider => {
                        GuardianReviewOutcome::Completed(assessment.clone())
                    }
                };
                async move {
                    (
                        outcome,
                        codex_analytics::GuardianReviewAnalyticsResult::without_session(),
                    )
                }
            };
            let (outcome, _) =
                super::run_review_attempt(&session, &context, deadline, &route, &mut run).await;
            if time_remains {
                let (next, _) =
                    super::run_review_attempt(&session, &context, deadline, &route, &mut run).await;
                let GuardianReviewOutcome::Completed(next) = next else {
                    panic!("sticky parent review must complete")
                };
                assert_eq!(next, assessment);
            }
            outcome
        };
        let after = Instant::now();
        let quarantine = &session
            .services
            .local_agent_runtime
            .reviewer_route_quarantine;
        let until = *quarantine.accounts.lock().await.get(&account_id).unwrap();
        assert!(until >= before + Duration::from_hours(/*hours*/ 1));
        assert!(until <= after + Duration::from_hours(/*hours*/ 1));
        assert_eq!(*route.lock().await, ReviewerRoute::ParentProvider);
        assert_eq!(
            quarantine.warned.lock().await.contains(&session.thread_id),
            time_remains
        );
        let mut warnings = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let EventMsg::GuardianWarning(warning) = event.msg {
                warnings.push(warning.message);
            }
        }
        if time_remains {
            let GuardianReviewOutcome::Completed(outcome) = outcome else {
                panic!("fallback must return parent assessment")
            };
            assert_eq!(outcome, assessment);
            assert_eq!(
                calls,
                [
                    chosen,
                    ReviewerRoute::ParentProvider,
                    ReviewerRoute::ParentProvider
                ]
            );
            assert_eq!(
                warnings,
                [
                    "Auto-review: OpenAI reviewer unavailable (unauthorized); using the Claude reviewer."
                ]
            );
        } else {
            assert!(
                matches!(outcome, GuardianReviewOutcome::Error(GuardianReviewError::Session {
                message, error_info: Some(CodexErrorInfo::Unauthorized), retry_at: None,
            }) if message == "original 401")
            );
            assert_eq!(calls, [chosen]);
            assert_eq!(warnings, [] as [String; 0]);
        }
    }
}
