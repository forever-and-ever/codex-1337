use super::*;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditResponse;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::GetWorkspaceMessagesResponse;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadAccountListResponse;
use codex_app_server_protocol::ThreadAccountSelectResponse;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use core_test_support::mock_server;
use pretty_assertions::assert_eq;
use wiremock::matchers::body_json;
use wiremock::matchers::header;

// A provider that does not use OpenAI authentication starts without any OpenAI login.
// Saved native identities, default preferences and captured conversation accounts
// remain distinct even without an OpenAI login.
#[tokio::test]
async fn claude_subscription_accounts_are_native_and_conversation_scoped() -> Result<()> {
    use codex_app_server_protocol::Account;
    use codex_app_server_protocol::AccountNicknameSetResponse;
    use codex_app_server_protocol::AccountProvider;
    use codex_app_server_protocol::ClaudeSubscriptionAccount;
    use codex_app_server_protocol::GetAccountResponse;
    use codex_login::anthropic::AccountIdentity;
    use codex_login::anthropic::AccountInfo;
    use codex_login::anthropic::AccountStore;
    use codex_login::anthropic::SubscriptionGrant;
    use codex_protocol::openai_models::AnthropicModelCapabilities;
    use codex_protocol::openai_models::AnthropicThinkingMode;

    let home = TempDir::new()?;
    let store = AccountStore::new(home.path().to_path_buf());
    let mut ids = Vec::new();
    let mut expected = Vec::new();
    for name in ["synthetic-a", "synthetic-b"] {
        let email = format!("{name}@example.test");
        let id = store
            .upsert(
                AccountInfo {
                    identity: AccountIdentity::new(name)?,
                    email: Some(email.clone()),
                    organization_uuid: None,
                    organization_name: None,
                },
                SubscriptionGrant::bearer_only(format!("not-a-real-token-{name}"))?,
            )
            .await?
            .to_string();
        expected.push(ClaudeSubscriptionAccount {
            id: id.clone(),
            email: Some(email),
            nickname: None,
            available: true,
            status: None,
        });
        ids.push(id);
    }
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    let mut catalog = codex_models_manager::bundled_models_response()?;
    catalog.models.truncate(1);
    catalog.models[0].slug = "claude-haiku-4-5-20251001".into();
    catalog.models[0].anthropic = Some(AnthropicModelCapabilities {
        thinking: AnthropicThinkingMode::Disabled,
        max_output_tokens: std::num::NonZeroU32::new(8192).unwrap(),
        supported_efforts: Vec::new(),
        supports_structured_outputs: false,
    });
    let catalog_path = home.path().join("models.json");
    std::fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
    let config_path = home.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"model = "claude-haiku-4-5-20251001"
model_provider = "claude_subscription"
model_catalog_json = {}
approval_policy = "never"
sandbox_mode = "danger-full-access"
[model_providers.claude_subscription]
name = "Claude subscription"
adapter = "claude_subscription"
base_url = "https://api.anthropic.com/v1"
requires_openai_auth = false
supports_websockets = false
[wrangler]
default_claude_account = {}
"#,
            serde_json::to_string(&catalog_path)?,
            serde_json::to_string(&ids[0])?,
        ),
    )?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let mut threads = Vec::new();
    for _ in 0..2 {
        let request = app
            .send_thread_start_request_with_auto_env(ThreadStartParams::default())
            .await?;
        let started: ThreadStartResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        threads.push(started.thread.id);
    }
    let request = app
        .send_request("thread/account/list", Some(json!({"threadId": threads[0]})))
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(accounts.claude_accounts, expected);
    assert_eq!(accounts.selected_claude_account_id, Some(ids[0].clone()));
    let request = app
        .send_request(
            "account/nickname/set",
            Some(json!({"provider":"claudeSubscription","accountId":ids[1],"nickname":"work"})),
        )
        .await?;
    let renamed: AccountNicknameSetResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        renamed,
        AccountNicknameSetResponse {
            provider: AccountProvider::ClaudeSubscription,
            account_id: ids[1].clone(),
            nickname: Some("work".into())
        }
    );
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId":threads[0],"provider":"claudeSubscription","accountId":ids[1]})),
        )
        .await?;
    let selected: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        selected,
        ThreadAccountSelectResponse {
            account_id: ids[1].clone(),
            provider: AccountProvider::ClaudeSubscription,
            conversation_changed: true,
            pending: false,
            default_saved: true
        }
    );
    let request = app
        .send_thread_fork_request(ThreadForkParams {
            thread_id: threads[0].clone(),
            ..Default::default()
        })
        .await?;
    let forked: ThreadForkResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let request = app
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let fresh: ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    for (thread, id, email, name) in [
        (
            &threads[0],
            &ids[1],
            "synthetic-b@example.test",
            Some("work"),
        ),
        (&threads[1], &ids[0], "synthetic-a@example.test", None),
        (
            &forked.thread.id,
            &ids[1],
            "synthetic-b@example.test",
            Some("work"),
        ),
        (
            &fresh.thread.id,
            &ids[1],
            "synthetic-b@example.test",
            Some("work"),
        ),
    ] {
        let request = app
            .send_request("account/read", Some(json!({"threadId":thread})))
            .await?;
        let account: GetAccountResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(
            account,
            GetAccountResponse {
                account_id: Some(id.clone()),
                user_id: None,
                account: Some(Account::ClaudeSubscription {
                    email: Some(email.to_owned())
                }),
                display_name: name.map(ToOwned::to_owned),
                requires_openai_auth: false,
                workspace_routing: None
            }
        );
    }
    let config: toml_edit::DocumentMut = std::fs::read_to_string(&config_path)?.parse()?;
    assert_eq!(
        config["wrangler"]["default_claude_account"].as_str(),
        Some(ids[1].as_str())
    );
    assert_eq!(store.list()?.len(), 2);
    assert!(app.shutdown_gracefully().await?.success());

    // Authoritative hydration must not disguise an expired grant as missing metadata.
    let account_path = home
        .path()
        .join("anthropic/accounts")
        .join(format!("{}.json", ids[1]));
    let mut record: serde_json::Value = serde_json::from_slice(&std::fs::read(&account_path)?)?;
    record["grant"]["expires_at"] = json!(1);
    std::fs::write(account_path, serde_json::to_vec(&record)?)?;
    let text = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        text.lines()
            .filter(|line| !line.starts_with("model_catalog_json"))
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let required = codex_protocol::sign_in_required::SignInRequired {
        provider: AccountProvider::ClaudeSubscription,
        account: "synthetic-b@example.test".into(),
    };
    for (method, params) in [
        ("thread/start", json!({})),
        ("thread/resume", json!({"threadId": threads[0]})),
    ] {
        let request = app.send_request(method, Some(params)).await?;
        let error = timeout(
            DEFAULT_READ_TIMEOUT,
            app.read_stream_until_error_message(RequestId::Integer(request)),
        )
        .await??;
        assert_eq!(error.error.code, -32600);
        assert_eq!(
            error.error.data,
            Some(
                json!({"codexErrorInfo": codex_app_server_protocol::CodexErrorInfo::SignInRequired(required.clone())})
            )
        );
        assert!(error.error.message.contains("synthetic-b@example.test"));
        assert!(
            error
                .error
                .message
                .contains("codex login --provider claude_subscription")
        );
    }
    assert!(app.shutdown_gracefully().await?.success());
    Ok(())
}

// Exercise the public boundary, not just credential storage: the payer follows
// the conversation through selection and forks, never the last global login.
#[tokio::test]
async fn conversation_accounts_survive_selection_login_and_fork() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    Box::pin(mock_server::run(&[&server], async {
    create_config_toml(
        home.path(),
        CreateConfigTomlParams {
            base_url: Some(server.uri()),
            chatgpt_base_url: Some(server.uri()),
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;
    for id in ["bob", "alice"] {
        write_chatgpt_auth(
            home.path(),
            ChatGptAuthFixture::new(format!("access-{id}"))
                .email(format!("{id}@example.com"))
                .plan_type("pro")
                .account_id(id)
                .chatgpt_account_id(id)
                .chatgpt_user_id(id),
            AuthCredentialsStoreMode::File,
        )?;
    }
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    Mock::given(method("GET"))
        .and(path("/api/codex/accounts/check"))
        .respond_with(|request: &wiremock::Request| {
            let account_id = request.headers["chatgpt-account-id"]
                .to_str()
                .expect("workspace account header");
            ResponseTemplate::new(200).set_body_json(json!({"accounts": [{
                "id": account_id,
                "workspace_backend_origin": "https://chatgpt.com",
                "account_routing_override": "NO_CONSTRAINT"
            }]}))
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(|request: &wiremock::Request| {
            let id = request.headers["chatgpt-account-id"]
                .to_str()
                .expect("quota account header");
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "account_id": id, "user_id": id, "plan_type": "pro",
                "rate_limit_reset_credits": {"available_count": 1},
                "rate_limit": {"allowed": true, "limit_reached": false,
                    "primary_window": {"used_percent": 10, "limit_window_seconds": 18000,
                        "reset_after_seconds": 1800, "reset_at": 2_000_000_000}}
            }))
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/workspace-messages"))
        .respond_with(|request: &wiremock::Request| {
            let id = request.headers["chatgpt-account-id"]
                .to_str()
                .expect("message account header");
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({"messages": [{
                "message_id": id, "message_type": "headline", "message_body": id
            }]}))
        })
        .mount(&server)
        .await;
    for id in ["alice", "bob"] {
        Mock::given(method("POST"))
            .and(path("/api/codex/rate-limit-reset-credits/consume"))
            .and(header("chatgpt-account-id", id))
            .and(header("authorization", format!("Bearer access-{id}")))
            .and(body_json(
                json!({"redeem_request_id": format!("reset-{id}")}),
            ))
            .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "code": "reset", "windows_reset": 1
            })))
            .mount(&server)
            .await;
    }
    let mut threads = Vec::new();
    for _ in 0..2 {
        let request = app
            .send_thread_start_request_with_auto_env(ThreadStartParams {
                config: Some([("features.apps".into(), json!(true))].into()),
                ..Default::default()
            })
            .await?;
        let response: ThreadStartResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        threads.push(response.thread.id);
    }
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": threads[0], "accountId": "bob:bob"})),
        )
        .await?;
    let selected: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        selected,
        ThreadAccountSelectResponse {
            account_id: "bob:bob".into(),
            provider: codex_app_server_protocol::AccountProvider::Openai,
            conversation_changed: true,
            pending: false,
            default_saved: true
        }
    );
    for (method, params) in [
            ("accountSession/list", json!({})),
            ("accountSession/switch", json!({"sessionId": "removed", "accountId": "alice"})),
        ] {
            let request = app.send_request(method, Some(params)).await?;
            let error = timeout(
                DEFAULT_READ_TIMEOUT,
                app.read_stream_until_error_message(RequestId::Integer(request)),
            ).await??;
            assert_eq!(error.error.code, -32600);
            assert_eq!(
                error.error.data,
                Some(json!({"reason": "workspaceSwitchingUnsupported"}))
            );
        }
    for method in [
        "account/read",
        "account/rateLimits/read",
        "account/rateLimitResetCredit/consume",
        "account/workspaceMessages/read",
    ] {
        let request = app
            .send_request(
                method,
                Some(json!({"threadId": threads[0], "newThread": {}, "idempotencyKey": "ambiguous-scope"})),
            )
            .await?;
        let error = timeout(
            DEFAULT_READ_TIMEOUT,
            app.read_stream_until_error_message(RequestId::Integer(request)),
        )
        .await??;
        assert_eq!(
            error.error,
            JSONRPCErrorError {
                code: -32600,
                message: "newThread cannot be combined with threadId".into(),
                data: None,
            }
        );
    }
    // Choosing an account makes it the active login, so drafts and unscoped reads agree.
    for (new_thread, id, account_id) in [
        (None, "bob", None),
        (
            Some(codex_app_server_protocol::NewThreadAccountParams {
                model_provider: None,
                cwd: None,
                config: None,
            }),
            "bob",
            Some("bob:bob".to_owned()),
        ),
    ] {
        // Identity, quota, spending and messages must share an account.
        let request = app
            .send_request(
                "account/rateLimits/read",
                Some(json!({
                    "newThread": new_thread, "excludeResetCreditDetails": true
                })),
            )
            .await?;
        let quota: GetAccountRateLimitsResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(quota.account_id.as_deref(), Some(id));
        let request = app
            .send_request(
                "account/workspaceMessages/read",
                Some(json!({"newThread": new_thread})),
            )
            .await?;
        let messages: GetWorkspaceMessagesResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert!(messages.feature_enabled);
        assert_eq!(messages.messages.len(), 1);
        assert_eq!(messages.messages[0].message_body, id);
        let request = app
            .send_request(
                "account/rateLimitResetCredit/consume",
                Some(json!({
                    "newThread": new_thread, "idempotencyKey": format!("reset-{id}"),
                    "confirmation": quota.reset_credit_confirmation
                })),
            )
            .await?;
        let consumed: ConsumeAccountRateLimitResetCreditResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(
            consumed,
            ConsumeAccountRateLimitResetCreditResponse {
                outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset,
            }
        );
        let request = app
            .send_get_account_request(GetAccountParams {
                new_thread,
                thread_id: None,
                refresh_token: false,
            })
            .await?;
        let response: GetAccountResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(
            response,
            GetAccountResponse {
                account_id,
                display_name: None,
                user_id: Some(id.to_string()),
                account: Some(Account::Chatgpt {
                    email: Some(format!("{id}@example.com")),
                    plan_type: AccountPlanType::Pro,
                    workspace_name: None,
                }),
                requires_openai_auth: true,
                workspace_routing: expected_workspace_routing(id),
            }
        );
    }
    // A draft confirmation owns its payer, not the login selected while its dialog is open.
    let request = app
        .send_request(
            "account/rateLimits/read",
            Some(json!({
                "newThread": {}, "excludeResetCreditDetails": true
            })),
        )
        .await?;
    let confirmed: GetAccountRateLimitsResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let confirmation = confirmed
        .reset_credit_confirmation
        .expect("draft confirmation");
    assert_eq!(
        confirmation.scope,
        codex_app_server_protocol::ResetCreditScope::NewThread {
            params: codex_app_server_protocol::NewThreadAccountParams {
                model_provider: None,
                cwd: None,
                config: None
            }
        }
    );
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_attempts = attempts.clone();
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(body_json(
            json!({"redeem_request_id": "pinned-draft", "credit_id": "draft-credit"}),
        ))
        .respond_with(move |request: &wiremock::Request| {
            assert_eq!(request.headers["chatgpt-account-id"], "bob");
            assert_eq!(request.headers["authorization"], "Bearer access-bob");
            if observed_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(json!({"code": "already_redeemed", "windows_reset": 0}))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({
                "threadId": threads[1], "accountId": "alice:alice"
            })),
        )
        .await?;
    let _: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let confirmed_attempt = json!({
        "newThread": {}, "confirmation": confirmation, "idempotencyKey": "pinned-draft", "creditId": "draft-credit"
    });
    let request = app
        .send_request(
            "account/rateLimitResetCredit/consume",
            Some(confirmed_attempt.clone()),
        )
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_error_message(RequestId::Integer(request)),
    )
    .await??;
    assert_eq!(error.error.code, -32603);
    let request = app
        .send_request(
            "account/rateLimitResetCredit/consume",
            Some(confirmed_attempt.clone()),
        )
        .await?;
    let retry: ConsumeAccountRateLimitResetCreditResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        retry.outcome,
        ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed
    );
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);

    // A valid token is not authority for a different conversation or a rewritten draft scope.
    let mut mismatched = confirmed_attempt;
    mismatched
        .as_object_mut()
        .expect("attempt object")
        .remove("newThread");
    mismatched["threadId"] = json!(threads[0]);
    let request = app
        .send_request("account/rateLimitResetCredit/consume", Some(mismatched))
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_error_message(RequestId::Integer(request)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    // A conversation's captured credentials cannot survive a payer switch on that conversation.
    let request = app
        .send_request(
            "account/rateLimits/read",
            Some(json!({
                "threadId": threads[0], "excludeResetCreditDetails": true
            })),
        )
        .await?;
    let confirmed: GetAccountRateLimitsResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let thread_confirmation = confirmed
        .reset_credit_confirmation
        .expect("conversation confirmation");
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({
                "threadId": threads[0], "accountId": "alice:alice"
            })),
        )
        .await?;
    let _: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "account/rateLimitResetCredit/consume",
            Some(json!({
                "threadId": threads[0], "confirmation": thread_confirmation,
                "idempotencyKey": "pinned-draft", "creditId": "draft-credit"
            })),
        )
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_error_message(RequestId::Integer(request)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({
                "threadId": threads[0], "accountId": "bob:bob"
            })),
        )
        .await?;
    let _: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    // The catalog follows the account: a lapsed former login once hid models that only the
    // chosen account's plan offers. An unscoped read speaks for the active login, and a
    // conversation's read for that conversation's own account.
    for id in ["bob", "alice"] {
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", format!("Bearer access-{id}")))
            .respond_with(
                ResponseTemplate::new(/*s*/ 200).set_body_json(ModelsResponse {
                    models: vec![serde_json::from_value::<ModelInfo>(json!({
                        "slug": format!("{id}-model"), "display_name": format!("{id}-model"),
                        "supported_reasoning_levels": [], "shell_type": "shell_command",
                        "visibility": "list", "supported_in_api": true, "priority": 0,
                        "support_verbosity": false,
                        "truncation_policy": {"mode": "bytes", "limit": 10000},
                        "experimental_supported_tools": []
                    }))?],
                }),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    for (thread_id, expected) in [(None, "bob-model"), (Some(&threads[1]), "alice-model")] {
        let models: ModelListResponse = app
            .request(|request_id| ClientRequest::ModelList {
                request_id,
                params: ModelListParams {
                    thread_id: thread_id.cloned(),
                    ..Default::default()
                },
            })
            .await?;
        assert_eq!(
            models
                .data
                .into_iter()
                .map(|model| model.id)
                .collect::<Vec<_>>(),
            [expected]
        );
    }
    let request = app
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let response: ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    threads.push(response.thread.id);

    // A new active login moves no conversation's binding.
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("access-alice-new")
            .email("alice@example.com")
            .plan_type("pro")
            .account_id("alice")
            .chatgpt_account_id("alice")
            .chatgpt_user_id("alice"),
        AuthCredentialsStoreMode::File,
    )?;
    for (thread, expected) in threads.iter().zip(["bob:bob", "alice:alice", "bob:bob"]) {
        let request = app
            .send_request("thread/account/list", Some(json!({"threadId": thread})))
            .await?;
        let response: ThreadAccountListResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(response.selected_account_id.as_deref(), Some(expected));
        assert_eq!(response.accounts.len(), 2);

        let request = app
            .send_get_account_request(GetAccountParams {
                new_thread: None,
                thread_id: Some(thread.clone()),
                refresh_token: false,
            })
            .await?;
        let response: GetAccountResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        let id = expected.split_once(':').expect("account selector").0;
        assert_eq!(
            response,
            GetAccountResponse {
                display_name: None,
                account_id: Some(expected.to_string()),
                user_id: Some(id.to_string()),
                account: Some(Account::Chatgpt {
                    email: Some(format!("{id}@example.com")),
                    plan_type: AccountPlanType::Pro,
                    workspace_name: None,
                }),
                requires_openai_auth: true,
                workspace_routing: expected_workspace_routing(id),
            }
        );
    }
    let request = app
        .send_request(
            "account/nickname/set",
            Some(json!({"provider": "openai", "accountId": "bob:bob", "nickname": "work"})),
        )
        .await?;
    let renamed: codex_app_server_protocol::AccountNicknameSetResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(renamed.nickname.as_deref(), Some("work"));
    let request = app
        .send_request("account/read", Some(json!({"threadId": threads[0]})))
        .await?;
    let renamed_account: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(renamed_account.display_name.as_deref(), Some("work"));

    let mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("payer"),
            responses::ev_completed("payer"),
        ]),
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: threads[0].clone(),
            input: vec![UserInput::Text {
                text: "Hello".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    assert_eq!(
        mock.single_request().header("authorization"),
        Some("Bearer access-bob".into())
    );
    assert_eq!(
        mock.single_request().header("chatgpt-account-id"),
        Some("bob".into())
    );

    // Metadata lookups must use the same identity as inference, not reopen the
    // singleton credential entry midway through the app request pipeline.
    Mock::given(method("POST"))
        .and(path("/ps/apps/batch"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer access-bob",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"apps": []})))
        .expect(1)
        .mount(&server)
        .await;
    let request = app
        .send_request(
            "app/read",
            Some(json!({"threadId": threads[0], "appIds": ["calendar"]})),
        )
        .await?;
    let response: codex_app_server_protocol::AppsReadResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(response.missing_app_ids, vec!["calendar"]);

    let initial = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("initial"),
            responses::ev_completed("initial"),
        ]),
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: threads[2].clone(),
            input: vec![UserInput::Text {
                text: "Persist the initial account".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    assert_eq!(
        initial.single_request().header("authorization"),
        Some("Bearer access-bob".into())
    );

    let request = app
        .send_thread_fork_request(ThreadForkParams {
            thread_id: threads[0].clone(),
            ..Default::default()
        })
        .await?;
    let fork: ThreadForkResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": threads[0], "accountId": "alice:alice"})),
        )
        .await?;
    let _: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "thread/account/list",
            Some(json!({"threadId": fork.thread.id})),
        )
        .await?;
    let response: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(response.selected_account_id.as_deref(), Some("bob:bob"));

    app.shutdown_gracefully().await?;
    app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    for (id, account) in [
        (&threads[0], "alice:alice"),
        (&fork.thread.id, "bob:bob"),
        (&threads[2], "bob:bob"),
    ] {
        let request = app
            .send_thread_resume_request(codex_app_server_protocol::ThreadResumeParams {
                thread_id: id.clone(),
                ..Default::default()
            })
            .await?;
        let _: codex_app_server_protocol::ThreadResumeResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        let request = app
            .send_request("thread/account/list", Some(json!({"threadId": id})))
            .await?;
        let response: ThreadAccountListResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(response.selected_account_id.as_deref(), Some(account));
    }

    // A live tool is a deterministic boundary: selection must not retarget it,
    // but its continuation must use the new payer without another user turn.
    let handoff = responses::mount_response_sequence(
        &server,
        vec![
            responses::sse_response(app_test_support::create_request_user_input_sse_response(
                "account-boundary",
            )?)
            .insert_header("x-codex-turn-state", "previous-payer-routing"),
            responses::sse_response(responses::sse(vec![responses::ev_completed("switched")])),
        ],
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: threads[0].clone(),
            input: vec![UserInput::Text {
                text: "Continue".into(),
                text_elements: Vec::new(),
            }],
            collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
                mode: codex_protocol::config_types::ModeKind::Plan,
                settings: codex_protocol::config_types::Settings {
                    model: "mock-model".into(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            }),
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let tool = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::ToolRequestUserInput {
        request_id: tool_id,
        ..
    } = tool
    else {
        anyhow::bail!("expected a user-input tool, got {tool:?}");
    };
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": threads[0], "accountId": "bob:bob"})),
        )
        .await?;
    let selected: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        selected,
        ThreadAccountSelectResponse {
            account_id: "bob:bob".into(),
            provider: codex_app_server_protocol::AccountProvider::Openai,
            conversation_changed: true,
            pending: true,
            default_saved: true
        },
    );
    let request = app
        .send_request("thread/account/list", Some(json!({"threadId": threads[0]})))
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("alice:alice"), Some("bob:bob"))
    );
    app.send_response(tool_id, json!({"answers": {}})).await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let requests = handoff.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].header("chatgpt-account-id"),
        Some("alice".into())
    );
    assert_eq!(requests[1].header("chatgpt-account-id"), Some("bob".into()));
    assert_eq!(
        requests[1].header("authorization"),
        Some("Bearer access-bob".into())
    );
    assert_eq!(requests[1].header("x-codex-turn-state"), None);
    assert!(
        requests[1]
            .function_call_output("account-boundary")
            .is_object()
    );
    let request = app
        .send_request("thread/account/list", Some(json!({"threadId": threads[0]})))
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("bob:bob"), None)
    );

    // Existing descendants roll independently even while their parent is blocked.
    let request = app
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            config: Some([("features.multi_agent_v2.enabled".into(), json!(true))].into()),
            approval_policy: Some(codex_app_server_protocol::AskForApproval::OnRequest),
            approvals_reviewer: Some(codex_app_server_protocol::ApprovalsReviewer::User),
            sandbox: Some(codex_app_server_protocol::SandboxMode::WorkspaceWrite),
            ..Default::default()
        })
        .await?;
    let family: ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    responses::mount_sse_once(&server, responses::sse(vec![
        responses::ev_function_call_with_namespace("spawn-family-worker", "wrangler_agents", "spawn_agent",
            &json!({"task_name": "worker", "message": "family-worker-only", "fork_turns": "none"}).to_string()),
        responses::ev_completed("family-spawn"),
    ])).await;
    let child = responses::mount_sse_once_match(&server, |request: &wiremock::Request| {
        let body = String::from_utf8_lossy(&request.body);
        body.contains("family-worker-only") && !body.contains("spawn-family-worker")
    }, responses::sse(vec![
        responses::ev_function_call("pause-family-worker", "exec_command",
            r#"{"cmd":"printf family-worker-done","sandbox_permissions":"require_escalated","justification":"account handoff approval boundary"}"#),
        responses::ev_completed("worker-approval"),
    ])).await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            String::from_utf8_lossy(&request.body).contains("spawn-family-worker")
        },
        app_test_support::create_request_user_input_sse_response("pause-family-root")?,
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: family.thread.id.clone(),
            input: vec![UserInput::Text {
                text: "Start the family".into(),
                text_elements: Vec::new(),
            }],
            collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
                mode: codex_protocol::config_types::ModeKind::Plan,
                settings: codex_protocol::config_types::Settings {
                    model: "mock-model".into(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            }),
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let mut questions = std::collections::HashMap::new();
    for _ in 0..2 {
        let request = timeout(
            DEFAULT_READ_TIMEOUT,
            app.read_stream_until_request_message(),
        )
        .await??;
        let (thread_id, request_id) = match request {
            ServerRequest::ToolRequestUserInput {
                request_id,
                params,
            } => (params.thread_id, request_id),
            ServerRequest::CommandExecutionRequestApproval {
                request_id,
                params,
            } => (params.thread_id, request_id),
            request => anyhow::bail!("expected family input request, got {request:?}"),
        };
        questions.insert(thread_id, request_id);
    }
    let root_question = questions.remove(&family.thread.id).expect("root question");
    let (child_id, child_question) = questions.into_iter().next().expect("child question");
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": family.thread.id, "accountId": "alice:alice"})),
        )
        .await?;
    let selected: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert!(selected.pending);
    let child_continued = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("worker-switched")]),
    )
    .await;
    app.send_response(child_question, json!({"decision": "decline"}))
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    assert_eq!(
        child.single_request().header("chatgpt-account-id"),
        Some("bob".into())
    );
    assert_eq!(
        child_continued
            .single_request()
            .header("chatgpt-account-id"),
        Some("alice".into())
    );
    let request = app
        .send_request("thread/account/list", Some(json!({"threadId": child_id})))
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("alice:alice"), None)
    );
    let request = app
        .send_request(
            "thread/account/list",
            Some(json!({"threadId": family.thread.id})),
        )
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("bob:bob"), Some("alice:alice"))
    );

    // Restart with the parent still waiting: its durable selection must not revert.
    app.shutdown_gracefully().await?;
    drop(root_question);
    app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let request = app
        .send_thread_resume_request(codex_app_server_protocol::ThreadResumeParams {
            thread_id: family.thread.id.clone(),
            ..Default::default()
        })
        .await?;
    let _: codex_app_server_protocol::ThreadResumeResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "thread/account/list",
            Some(json!({"threadId": family.thread.id})),
        )
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("alice:alice"), None)
    );
    // Retry backoff is a request boundary too: never require an overloaded
    // account to recover before the requested replacement can be used.
    let retried = responses::mount_response_sequence(
        &server,
        vec![
            ResponseTemplate::new(503)
                .set_body_json(json!({"error": {"code": "server_is_overloaded"}})),
            responses::sse_response(responses::sse(vec![responses::ev_completed(
                "new-account-retry",
            )])),
        ],
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: family.thread.id.clone(),
            input: vec![UserInput::Text {
                text: "Continue after capacity".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let retry: codex_app_server_protocol::ErrorNotification =
        timeout(DEFAULT_READ_TIMEOUT, app.read_notification("error")).await??;
    assert!(retry.will_retry);
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": family.thread.id, "accountId": "bob:bob"})),
        )
        .await?;
    let _: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    assert_eq!(
        retried
            .requests()
            .iter()
            .map(|request| request.header("chatgpt-account-id"))
            .collect::<Vec<_>>(),
        vec![Some("alice".into()), Some("bob".into())]
    );

    // Legacy children accept account configuration directly. A child-origin
    // switch must checkpoint the idle root, not merely the child's rollout.
    let request = app
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            config: Some([("features.multi_agent_v2.enabled".into(), json!(false))].into()),
            approval_policy: Some(codex_app_server_protocol::AskForApproval::OnRequest),
            approvals_reviewer: Some(codex_app_server_protocol::ApprovalsReviewer::User),
            sandbox: Some(codex_app_server_protocol::SandboxMode::WorkspaceWrite),
            ..Default::default()
        })
        .await?;
    let legacy: ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_function_call_with_namespace(
                "spawn-account-child",
                "multi_agent_v1",
                "spawn_agent",
                &json!({"message": "legacy-account-child-only", "fork_context": false}).to_string(),
            ),
            responses::ev_completed("legacy-spawn"),
        ]),
    )
    .await;
    responses::mount_sse_once_match(&server, |request: &wiremock::Request| {
        let body = String::from_utf8_lossy(&request.body);
        body.contains("legacy-account-child-only") && !body.contains("spawn-account-child")
    }, responses::sse(vec![
        responses::ev_function_call("hold-account-child", "exec_command",
            r#"{"cmd":"printf legacy-child","sandbox_permissions":"require_escalated","justification":"account durability approval boundary"}"#),
        responses::ev_completed("legacy-child-approval"),
    ])).await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            String::from_utf8_lossy(&request.body).contains("spawn-account-child")
        },
        responses::sse(vec![responses::ev_completed("idle-account-root")]),
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: legacy.thread.id.clone(),
            input: vec![UserInput::Text {
                text: "Start legacy account family".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let mut root_idle = false;
    let mut legacy_child = None;
    while !root_idle || legacy_child.is_none() {
        match timeout(DEFAULT_READ_TIMEOUT, app.read_next_message()).await?? {
            codex_app_server_protocol::JSONRPCMessage::Notification(notification)
                if notification.method == "turn/completed"
                    && notification
                        .params
                        .as_ref()
                        .is_some_and(|params| params["threadId"] == legacy.thread.id) =>
            {
                root_idle = true;
            }
            codex_app_server_protocol::JSONRPCMessage::Request(request)
                if request.method == "item/commandExecution/requestApproval" =>
            {
                legacy_child = request
                    .params
                    .and_then(|params| params["threadId"].as_str().map(str::to_owned));
            }
            _ => {}
        }
    }
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({
                "threadId": legacy_child.expect("legacy child approval"), "accountId": "alice:alice"
            })),
        )
        .await?;
    let selected: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert!(selected.conversation_changed);
    assert!(selected.default_saved);
    assert!(selected.pending);
    app.shutdown_gracefully().await?;
    // Another family's choice of active login cannot supersede the saved root selection.
    codex_login::auth::activate_account(
        home.path(),
        "bob:bob",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let request = app
        .send_thread_resume_request(codex_app_server_protocol::ThreadResumeParams {
            thread_id: legacy.thread.id.clone(),
            ..Default::default()
        })
        .await?;
    let _: codex_app_server_protocol::ThreadResumeResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "thread/account/list",
            Some(json!({"threadId": legacy.thread.id})),
        )
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("alice:alice"), None)
    );
    Ok(())
    }))
    .await
}
