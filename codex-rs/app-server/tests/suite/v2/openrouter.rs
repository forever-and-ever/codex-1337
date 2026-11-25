use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ErrorNotification;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::OpenRouterKeyAddResponse;
use codex_app_server_protocol::OpenRouterKeyState;
use codex_app_server_protocol::OpenRouterKeyStatusResponse;
use codex_app_server_protocol::OpenRouterModelEndpointsResponse;
use codex_app_server_protocol::OpenRouterModelForgetResponse;
use codex_app_server_protocol::OpenRouterModelRouteResponse;
use codex_app_server_protocol::OpenRouterModelSaveResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_protocol::KeyName;
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

#[path = "openrouter_batch_recovery.rs"]
mod batch_recovery;

async fn mount_routed_catalog(server: &MockServer, model: &str, host: &str) {
    Mock::given(method("GET"))
            .and(path(format!("/model/{model}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
                "id": model, "name": "Routing Test", "canonical_slug":model, "endpoints":["/api/v1/responses"],
                "context_length": 262_144, "architecture": {"input_modalities": ["text","image"]},
                "supported_parameters": ["tools"], "pricing": {"prompt": "0.0000003", "completion": "0.000002"}
            }})))
            .mount(server)
            .await;
    Mock::given(method("GET"))
            .and(path(format!("/models/{model}/endpoints")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"endpoints": [{
                "tag": format!("{host}/test"), "provider_name": host, "supported_parameters":["tools"],
                "quantization": "fp8", "service_tier": "priority", "context_length": 131_072,
                "pricing": {"prompt": "0.0000003", "completion": "0.000002"},
                "uptime_last_30m": 99.5, "status": 0
            }]}})))
            .mount(server)
            .await;
    let batch_alias = format!("{model}:batch");
    Mock::given(method("GET"))
        .and(path("/models/user"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":model},{"id":batch_alias}]})),
        )
        .mount(server)
        .await;
    Mock::given(method("GET")).and(path(format!("/model/{batch_alias}"))).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":{
            "id":batch_alias,"name":"Routing Test (batch)","canonical_slug":model,"endpoints":["/api/v1/batches"],"context_length":131_072,
            "architecture":{"input_modalities":["text","image"]},"supported_parameters":["tools"],"pricing":{}
        }}))).mount(server).await;
    Mock::given(method("GET")).and(path(format!("/models/{batch_alias}/endpoints"))).respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":{"endpoints":[{
            "tag":host,"provider_name":host,"context_length":131_072,"pricing":{},"status":0,"supported_parameters":["tools"]
        }]}}))).mount(server).await;
}

#[tokio::test]
async fn openrouter_saved_model_routes_spend_and_key_quota() -> Result<()> {
    for (model, host) in [
        ("moonshotai/kimi-test", "moonshotai"),
        ("google/gemini-test", "google"),
    ] {
        check_saved_model_routes_spend_and_key_quota(model, host).await?;
    }
    Ok(())
}

// The public configuration path must reach the same catalog, payer and routing
// used by a live turn; checking only the request handlers cannot prove that.
async fn check_saved_model_routes_spend_and_key_quota(model: &str, host: &str) -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    let (filter, routing) = if model.starts_with("google/") {
        (
            json!({"ignore": [host]}),
            json!({"sort": "exacto", "ignore": [host]}),
        )
    } else {
        (
            json!({"only": [host]}),
            json!({"sort": "exacto", "only": [host]}),
        )
    };
    Box::pin(mock_server::run(&[&server], async {
        let key_path = home.path().join("openrouter.key");
        std::fs::write(&key_path, "sk-or-test\n")?;
        let config_path = home.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"model_provider = "openrouter"
model = "{model}"
approval_policy = "never"
sandbox_mode = "danger-full-access"
[model_providers.openrouter]
name = "OpenRouter"
adapter = "openrouter"
base_url = {}
requires_openai_auth = false
supports_websockets = false
request_max_retries = 0
stream_max_retries = 2
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
        mount_routed_catalog(&server,model,host).await;
        let mut completed = responses::ev_completed_with_tokens("paid-turn", 123);
        let batch_alias=format!("{model}:batch");
        completed["response"]["usage"]["cost"] = json!(0.0123);
        // Item completion is not response completion. Exercise both a provider
        // timeout and a closed stream, before and after a non-idempotent tool.
        let unfinished = |id: &str| responses::sse(vec![
            responses::ev_response_created(id),
            responses::ev_assistant_message(&format!("{id}-message"), "Partial answer."),
            responses::ev_reasoning_item(&format!("{id}-thought"), &[], &["Unfinished reasoning."]),
        ]);
        let timed_out = |id: &str| format!(
            "{}{}",
            unfinished(id),
            responses::sse_failed(id, "server_error", "Upstream idle timeout exceeded"),
        );
        let wire = responses::mount_response_sequence(
            &server,
            vec![
                responses::sse_response(timed_out("before-tool")),
                responses::sse_response(responses::sse(vec![
                    responses::ev_response_created("tool-turn"),
                    responses::ev_function_call("retry-effect", "exec_command", &json!({
                        "cmd": "printf 'executed\\n' >> retry-effect",
                        "workdir": home.path(),
                        "max_output_tokens": 1000
                    }).to_string()),
                    responses::ev_completed("tool-turn"),
                ])),
                responses::sse_response(timed_out("after-tool")),
                responses::sse_response(responses::sse(vec![
                    responses::ev_response_created("closed-stream"),
                    responses::ev_reasoning_item("closed-stream-thought", &[], &["Unfinished reasoning."]),
                    responses::ev_assistant_message("closed-stream-message", "Partial answer."),
                ])),
                responses::sse_response(responses::sse(vec![
                    responses::ev_response_created("paid-turn"),
                    completed,
                ])),
                ResponseTemplate::new(402).set_body_json(json!({
                    "error": {"message": "Insufficient credits", "code": 402}
                })),
                responses::sse_response(responses::sse(vec![
                    responses::ev_response_created("spare-turn"),
                    responses::ev_completed("spare-turn"),
                ])),
                ResponseTemplate::new(422).set_body_json(json!({"error": {
                    "message": "Host failed", "metadata": {
                        "provider_name": host,
                        "raw": json!({"error": {"message": "Only function tools are supported"}}).to_string()
                    }
                }})),
                responses::sse_response(responses::sse(vec![responses::ev_completed("resumed-turn")])),
                responses::sse_response(responses::sse(vec![responses::ev_completed("resaved-turn")])),
                responses::sse_response(responses::sse(vec![responses::ev_completed("rerouted-turn")])),
                responses::sse_response(responses::sse(vec![responses::ev_completed("direct-turn")])),
                responses::sse_response(responses::sse(vec![responses::ev_completed("preset-turn")])),
            ],
        )
        .await;
        // Google rejects assistant-prefilled requests rather than resuming them.
        Mock::given(method("POST"))
            .and(|request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).expect("request JSON");
                body["input"].as_array().and_then(|input| input.last()).is_some_and(|item| {
                    item["type"] == "reasoning" || item["role"] == "assistant"
                })
            })
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
                "message": "Host failed", "metadata": {
                    "provider_name": "Google",
                    "raw": json!({"error": {"message": "Requests ending with a model turn are not supported."}}).to_string()
                }
            }})))
            .with_priority(1)
            .expect(0)
            .mount(&server)
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
            "slug": model
        }))).await?;
        let saved: OpenRouterModelSaveResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(saved.model.model, model);
        let request = app.send_thread_start_request_with_auto_env(ThreadStartParams::default()).await?;
        let started: ThreadStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let request = app.send_request("thread/account/list", Some(json!({"threadId": started.thread.id}))).await?;
        let accounts: codex_app_server_protocol::ThreadAccountListResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(accounts.conversation_provider, Some(codex_app_server_protocol::AccountProvider::OpenRouter));
        assert_eq!(accounts.selected_openrouter_key.as_ref().map(KeyName::as_str), Some("main"));
        assert_eq!(accounts.pending_openrouter_key, None);
        assert_eq!(accounts.openrouter_keys.len(), 1);
        for scope in [json!({"threadId": started.thread.id}), json!({"newThread": {"modelProvider": "openrouter"}})] {
            let request = app.send_request("account/read", Some(scope)).await?;
            let account: codex_app_server_protocol::GetAccountResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
            assert_eq!(account.account_id.as_deref(), Some("main"));
            assert!(account.display_name.is_none());
            assert!(account.account.is_none());
            assert!(!account.requires_openai_auth);
        }
        let request = app.send_request("openrouter/model/endpoints", Some(json!({
            "slug": model
        }))).await?;
        let endpoints: OpenRouterModelEndpointsResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(endpoints.hosts[0].slug.as_str(), host);
        assert_eq!(endpoints.hosts[0].endpoints[0].prompt_per_mtok, Some(Usd::from_nanodollars(300_000_000)));
        let request = app.send_request("openrouter/model/route", Some(json!({
            "slug": model, "filter": filter
        }))).await?;
        let _: OpenRouterModelRouteResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let request = app.send_request("model/list", Some(json!({"modelProvider": "openrouter"}))).await?;
        let listed: ModelListResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let batch_eligible = filter.get("only").is_some();
        let expected = if batch_eligible { vec![model, batch_alias.as_str()] } else { vec![model] };
        assert_eq!(listed.data.iter().map(|row| row.model.as_str()).collect::<Vec<_>>(), expected);
        assert_eq!(listed.data[0].model, saved.model.model);
        assert!(listed.providers.iter().any(|provider| provider.id == "openrouter"));
        let stored: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
        let filter_kind = if filter.get("only").is_some() { "only" } else { "ignore" };
        assert_eq!(stored["wrangler"]["openrouter"]["models"][model][filter_kind][0].as_str(), Some(host));
        assert!(!std::fs::read_to_string(&config_path)?.contains("sk-or-test"));

        let request = app.send_turn_start_request(TurnStartParams {
            thread_id: started.thread.id.clone(),
            input: vec![UserInput::Text { text: "Reply briefly.".into(), text_elements: Vec::new() }],
            ..Default::default()
        }).await?;
        let _: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let usage = timeout(READ_TIMEOUT, async {
            loop {
                let usage: ThreadTokenUsageUpdatedNotification =
                    app.read_notification("thread/tokenUsage/updated").await?;
                if usage.token_usage.total.cost.is_some() {
                    break Ok::<_, anyhow::Error>(usage);
                }
            }
        }).await??;
        assert_eq!(usage.token_usage.total.cost, Some(Usd::from_nanodollars(12_300_000)));
        assert_eq!(usage.token_usage.last.cost, usage.token_usage.total.cost);
        let completed: TurnCompletedNotification = timeout(READ_TIMEOUT, app.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
        let requests = wire.requests();
        assert_eq!(requests.len(), 5);
        assert_eq!(std::fs::read_to_string(home.path().join("retry-effect"))?, "executed\n");
        // Recovery retains original assistant/reasoning items and adds exactly one
        // request-only harness notice. Repeated retries must not accumulate fake turns.
        for index in [1, 3, 4] {
            let body = requests[index].body_json();
            let input = body["input"].as_array().expect("retry input");
            let notice = input.last().expect("continuation notice");
            assert_eq!(notice["role"], "user");
            let text = notice["content"][0]["text"].as_str().expect("notice text");
            assert!(text.starts_with("<harness_continuation>"));
            // Recovery asks Google for a small next step without constraining other
            // hosts, normal requests, or the subsequent tool-result continuation.
            assert_eq!(text.contains("Keep the next tool input under 2000 characters"), model.starts_with("google/"));
            let preceding = &input[input.len() - 3..input.len() - 1];
            assert!(preceding.iter().any(|item| item["type"] == "reasoning"));
            assert!(preceding.iter().any(|item| item["role"] == "assistant"));
            assert_eq!(input.iter().filter(|item| item.to_string().contains("<harness_continuation>")).count(), 1);
        }
        let settled_input = requests[2].body_json()["input"].clone();
        assert!(!settled_input.to_string().contains("<harness_continuation>"));
        assert!(settled_input.to_string().contains("Unfinished reasoning."));
        let input = settled_input.as_array().expect("settled input");
        assert_eq!(input.last().expect("tool result")["type"], "function_call_output");
        assert_eq!(input.iter().filter(|item| item["type"] == "function_call" && item["call_id"] == "retry-effect").count(), 1);
        assert_eq!(input.iter().filter(|item| item["type"] == "function_call_output" && item["call_id"] == "retry-effect").count(), 1);
        let request = &requests[0];
        assert_eq!(request.header("authorization"), Some("Bearer sk-or-test".into()));
        let body = request.body_json();
        assert!(!body["input"].to_string().contains("<harness_continuation>"));
        for request in &requests {
            assert_eq!(request.body_json()["provider"], routing);
        }
        assert_eq!(body["model"], model);
        assert_eq!(body["store"], false);
        assert!(body["tools"].as_array().expect("request tools").iter().all(|tool| tool["type"] == "function"));

        if batch_eligible {
            // The same agent must execute the returned call and carry its real receipt into
            // the next paid row; streaming the alias to /responses would miss this contract.
            let jobs=std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
            let submitted=jobs.clone();
            Mock::given(method("POST")).and(path("/batches")).and(header("authorization","Bearer sk-or-test"))
                .respond_with(move |r:&wiremock::Request|{
                    let mut rows=submitted.lock().expect("mock jobs"); rows.push(r.body_json().expect("valid JSON request"));
                    ResponseTemplate::new(202).set_body_json(json!({"id":format!("batch_{}",rows.len())}))
                }).expect(2).mount(&server).await;
            let polled=jobs.clone();
            let effect_dir=home.path().to_owned();
            Mock::given(method("GET")).and(wiremock::matchers::path_regex("^/batches/batch_[12]$"))
                .respond_with(move|r:&wiremock::Request|{
                    let step=usize::from(r.url.path().ends_with('2'));
                    let call=if step==0 {json!({"type":"function_call","id":"batch-tool","call_id":"batch-effect","name":"exec_command","arguments":json!({"cmd":"printf x >> batch-effect","workdir":effect_dir}).to_string(),"status":"completed"})}
                        else {json!({"type":"message","id":"batch-message","role":"assistant","content":[{"type":"output_text","text":"Done."}],"status":"completed"})};
                    let rows=polled.lock().expect("mock jobs");
                    ResponseTemplate::new(200).set_body_json(json!({"id":format!("batch_{}",step+1),"status":"completed",
                        "usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost":0.01},
                        "results":[{"custom_id":rows[step]["requests"][0]["custom_id"],"response":{"status_code":200,"body":{"status":"completed","id":format!("slow-{step}"),"output":[call]}}}]}))
                }).mount(&server).await;
            Mock::given(method("DELETE")).and(wiremock::matchers::path_regex("^/batches/batch_[12]$")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"deleted":true}))).expect(2).mount(&server).await;
            let request=app.send_request("thread/settings/update",Some(json!({"threadId":started.thread.id,"openrouterTransport":"batch"}))).await?;
            let ack:codex_app_server_protocol::ThreadSettingsUpdateResponse=timeout(READ_TIMEOUT,app.read_response(request)).await??;
            assert_eq!(ack.openrouter_transport,Some(codex_protocol::openrouter::OpenRouterTransport::Batch));
            let request=app.send_turn_start_request(TurnStartParams{thread_id:started.thread.id.clone(),input:vec![UserInput::Text{text:"Run a batch tool.".into(),text_elements:Vec::new()}],..Default::default()}).await?;
            let _:TurnStartResponse=timeout(READ_TIMEOUT,app.read_response(request)).await??;
            let slow_done:TurnCompletedNotification=timeout(READ_TIMEOUT,app.read_notification("turn/completed")).await??;
            assert_eq!(slow_done.turn.status,TurnStatus::Completed);
            assert_eq!(std::fs::read_to_string(home.path().join("batch-effect"))?,"x");
            {
                let rows=jobs.lock().expect("mock jobs"); assert_eq!(rows.len(),2);
                for job in rows.iter(){
                    assert_eq!(job["endpoint"],"/v1/responses"); assert_eq!(job["model"],model);
                    assert_eq!(job["provider"],json!({"only":[host],"allow_fallbacks":false}));
                    let row=&job["requests"][0]["body"];assert_eq!(row["model"],model); assert_eq!(row["stream"],false);
                    assert_eq!(row.get("prompt_cache_key"),body.get("prompt_cache_key"),"transport must not change cache identity");
                    assert_eq!(row.get("prompt_cache_options"),body.get("prompt_cache_options"));
                    assert_eq!(row.get("reasoning"),body.get("reasoning"),"transport must not change reasoning effort");
                    assert!(row.get("provider").is_none() && row.get("stream_options").is_none());
                }
                assert!(rows[1]["requests"][0]["body"]["input"].as_array().expect("input items").iter().any(|item|item["type"]=="function_call_output" && item["call_id"]=="batch-effect"));
            }
        }

        // Removing a picker entry must not widen the route of an already admitted
        // thread, including after changing keys or rebuilding its runtime.
        let request = app.send_request("openrouter/model/forget", Some(json!({"slug": model}))).await?;
        let _: OpenRouterModelForgetResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let request = app.send_request("model/list", Some(json!({"modelProvider": "openrouter"}))).await?;
        let listed: ModelListResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert!(!listed.data.iter().any(|entry| entry.model == model || entry.model == batch_alias));

        if batch_eligible {
            // The retained live filter is not a saved batch admission. An existing slow
            // thread must stop rather than buy live inference after the picker entry vanishes.
            let request = app.send_turn_start_request(TurnStartParams {
                thread_id: started.thread.id.clone(),
                input: vec![UserInput::Text { text: "Continue after forget.".into(), text_elements: Vec::new() }],
                ..Default::default()
            }).await?;
            let _: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
            let stopped: TurnCompletedNotification = timeout(READ_TIMEOUT, app.read_notification("turn/completed")).await??;
            assert_eq!(stopped.turn.status, TurnStatus::Failed);
            assert_eq!(wire.requests().len(), 5);
        }
        let request = app.send_request("thread/settings/update", Some(json!({
            "threadId": started.thread.id, "openrouterTransport": "batch"
        }))).await?;
        let error = timeout(READ_TIMEOUT, app.read_stream_until_error_message(codex_app_server_protocol::RequestId::Integer(request))).await??;
        assert!(error.error.message.contains("save the ordinary model"), "{error:?}");
        let request=app.send_request("thread/settings/update",Some(json!({"threadId":started.thread.id,"openrouterTransport":"live"}))).await?;
        let _:codex_app_server_protocol::ThreadSettingsUpdateResponse=timeout(READ_TIMEOUT,app.read_response(request)).await??;

        let request = app.send_turn_start_request(TurnStartParams {
            thread_id: started.thread.id.clone(),
            input: vec![UserInput::Text { text: "Use the exhausted key.".into(), text_elements: Vec::new() }],
            ..Default::default()
        }).await?;
        let exhausted: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let error = timeout(READ_TIMEOUT, async {
            loop {
                let error: ErrorNotification = app.read_notification("error").await?;
                if !error.will_retry && error.turn_id == exhausted.turn.id {
                    break Ok::<_, anyhow::Error>(error);
                }
            }
        }).await??;
        assert_eq!(error.error.codex_error_info, Some(codex_app_server_protocol::CodexErrorInfo::UsageLimitExceeded));
        assert!(error.error.message.contains("main"));
        assert!(error.error.message.contains("/account"));
        let completed: TurnCompletedNotification = timeout(READ_TIMEOUT, app.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Failed);
        assert_eq!(wire.requests().len(), 6);
        // The following real user turn still sees the saved assistant context, but
        // never any synthetic user message from earlier request projections.
        let next_input = wire.requests()[5].body_json()["input"].clone();
        assert!(next_input.to_string().contains("Unfinished reasoning."));
        assert!(!next_input.to_string().contains("<harness_continuation>"));

        let spare_path = home.path().join("openrouter-spare.key");
        std::fs::write(&spare_path, "sk-or-spare\n")?;
        Mock::given(method("GET"))
            .and(path("/key"))
            .and(header("authorization", "Bearer sk-or-spare"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
                "label": "…pare", "limit": null, "limit_remaining": null,
                "limit_reset": null, "usage_daily": 0, "usage_weekly": 0,
                "usage_monthly": 0, "expires_at": null
            }})))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/credits"))
            .and(header("authorization", "Bearer sk-or-spare"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"data": {"total_credits": 10, "total_usage": 0}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let request = app.send_request("openrouter/key/add", Some(json!({
            "name": "spare", "path": spare_path
        }))).await?;
        let added: OpenRouterKeyAddResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(added.key.name.as_str(), "spare");
        assert!(!added.key.is_default);

        let request = app.send_request("thread/account/select", Some(json!({
            "threadId": started.thread.id, "provider": "openrouter", "accountId": "spare"
        }))).await?;
        let selected: codex_app_server_protocol::ThreadAccountSelectResponse =
            timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert!(selected.conversation_changed);
        assert!(selected.default_saved);
        assert!(!selected.pending);
        let request = app.send_request("thread/account/list", Some(json!({
            "threadId": started.thread.id
        }))).await?;
        let accounts: codex_app_server_protocol::ThreadAccountListResponse =
            timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(accounts.selected_openrouter_key.as_ref().map(KeyName::as_str), Some("spare"));
        assert_eq!(accounts.openrouter_keys.iter().map(|status| status.key.name.as_str()).collect::<Vec<_>>(),
            vec!["main", "spare"]);

        let request = app.send_turn_start_request(TurnStartParams {
            thread_id: started.thread.id.clone(),
            input: vec![UserInput::Text { text: "Continue with the spare key.".into(), text_elements: Vec::new() }],
            ..Default::default()
        }).await?;
        let _: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let completed: TurnCompletedNotification =
            timeout(READ_TIMEOUT, app.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
        let requests = wire.requests();
        assert_eq!(requests.len(), 7);
        assert_eq!(requests[6].header("authorization"), Some("Bearer sk-or-spare".into()));
        for request in &requests[5..] {
            assert_eq!(request.body_json()["provider"], routing);
        }
        let request = app.send_turn_start_request(TurnStartParams {
            thread_id: started.thread.id.clone(),
            input: vec![UserInput::Text { text: "Check the host rejection.".into(), text_elements: Vec::new() }],
            ..Default::default()
        }).await?;
        let _: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        let error: ErrorNotification = timeout(READ_TIMEOUT, app.read_notification("error")).await??;
        assert!(!error.will_retry);
        assert!(error.error.message.contains(&format!("OpenRouter host {host} rejected the request: Only function tools are supported")));
        let completed: TurnCompletedNotification = timeout(READ_TIMEOUT, app.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Failed);
        assert_eq!(wire.requests().len(), 8);
        assert_eq!(wire.requests()[7].body_json()["provider"], routing);

        drop(app);
        let mut app = TestAppServer::builder()
            .with_codex_home(home.path())
            .with_env_overrides(&[("OPENAI_API_KEY", None)])
            .build_initialized_with_timeout(READ_TIMEOUT)
            .await?;
        let request = app.send_thread_resume_request(ThreadResumeParams {
            thread_id: started.thread.id.clone(),
            ..Default::default()
        }).await?;
        let resumed: ThreadResumeResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(resumed.thread.id, started.thread.id);
        complete_turn(&mut app, &resumed.thread.id).await?;
        assert_eq!(wire.requests()[8].body_json()["provider"], routing);

        let request = app.send_request("openrouter/model/save", Some(json!({"slug": model}))).await?;
        let _: OpenRouterModelSaveResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        complete_turn(&mut app, &resumed.thread.id).await?;
        assert_eq!(wire.requests()[9].body_json()["provider"], routing);

        let request = app.send_request("openrouter/model/route", Some(json!({
            "slug": model, "filter": {}
        }))).await?;
        let _: OpenRouterModelRouteResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
        complete_turn(&mut app, &resumed.thread.id).await?;
        assert_eq!(wire.requests()[10].body_json()["provider"], json!({"sort": "exacto"}));

        for (index, (model, provider)) in [
            ("direct/never-saved", json!({"sort": "exacto"})),
            ("@preset/never-saved", serde_json::Value::Null),
        ].into_iter().enumerate() {
            let request = app.send_thread_start_request_with_auto_env(ThreadStartParams {
                model: Some(model.into()),
                ..Default::default()
            }).await?;
            let started: ThreadStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
            complete_turn(&mut app, &started.thread.id).await?;
            let body = wire.requests()[11 + index].body_json();
            assert_eq!(body["model"], model);
            assert_eq!(body["provider"], provider);
        }
        Ok(())
    })).await
}

async fn complete_turn(app: &mut TestAppServer, thread_id: &str) -> Result<()> {
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.into(),
            input: vec![UserInput::Text {
                text: "Reply briefly.".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(READ_TIMEOUT, app.read_response(request)).await??;
    let completed: TurnCompletedNotification =
        timeout(READ_TIMEOUT, app.read_notification("turn/completed")).await??;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    Ok(())
}
