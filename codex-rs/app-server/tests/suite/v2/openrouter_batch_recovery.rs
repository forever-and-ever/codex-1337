//! Paid jobs outlive their originating connection and managed daemon. Drive the
//! real farm, approval, compact and continuation paths against a gated provider.
use super::super::daemon_update_recovery::connect_daemon_client;
use super::super::daemon_update_recovery::request;
use super::super::daemon_update_recovery::request_shutdown;
use super::super::daemon_update_recovery::spawn_server;
use super::super::daemon_update_recovery::spawn_server_with_stderr;
use super::super::daemon_update_recovery::wait_success;
use super::READ_TIMEOUT;
use super::mount_routed_catalog;
use anyhow::Context;
use anyhow::Result;
use codex_app_server_protocol::InitializeCapabilities;
use core_test_support::mock_server;
use futures::SinkExt;
use futures::StreamExt;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::path_regex;

const MODEL: &str = "google/gemini-test";

fn configure(home: &TempDir, provider: &MockServer, host: &str) -> Result<()> {
    let key = home.path().join("key");
    std::fs::write(&key, "sk-or-restart\n")?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"model="{}"
model_provider="openrouter"
approval_policy="on-request"
sandbox_mode="workspace-write"
model_auto_compact_token_limit=90000
[features]
multi_agent_v2=true
[wrangler.openrouter]
default_key="mock"
[wrangler.openrouter.key_files]
mock={}
[wrangler.openrouter.models."{}"]
only=["{host}"]
[wrangler.farm]
model="{}"
[model_providers.openrouter]
name="OpenRouter"
adapter="openrouter"
base_url={}
request_max_retries=0
stream_max_retries=0
"#,
            MODEL,
            serde_json::to_string(&key)?,
            MODEL,
            MODEL,
            serde_json::to_string(&provider.uri())?
        ),
    )?;
    Ok(())
}

#[tokio::test]
async fn openrouter_batch_recovers_farm_tool_and_compaction() -> Result<()> {
    let home = TempDir::new()?;
    let provider = MockServer::start().await;
    Box::pin(mock_server::run(
        &[&provider],
        recover_farm(&home, &provider),
    ))
    .await
}

async fn recover_farm(home: &TempDir, provider: &MockServer) -> Result<()> {
    mount_routed_catalog(provider, MODEL, "google").await;
    configure(home, provider, "google")?;
    let jobs = Arc::new(Mutex::new(Vec::<Value>::new()));
    let posted = Arc::clone(&jobs);
    Mock::given(method("POST"))
        .and(path("/batches"))
        .and(header("authorization", "Bearer sk-or-restart"))
        .respond_with(move |request: &wiremock::Request| {
            let mut rows = posted.lock().expect("jobs");
            rows.push(request.body_json().expect("paid request"));
            ResponseTemplate::new(202).set_body_json(json!({"id":format!("batch_{}",rows.len())}))
        })
        .mount(provider)
        .await;
    let first_read = Arc::new(Notify::new());
    let restored_read = Arc::new(Notify::new());
    let restored = Arc::new(AtomicBool::new(false));
    let epoch = Arc::clone(&restored);
    let reattached = Arc::clone(&restored_read);
    let release = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&release);
    let notified = Arc::clone(&first_read);
    let rows = Arc::clone(&jobs);
    let cwd = home.path().to_owned();
    Mock::given(method("GET")).and(path_regex("^/batches/batch_[0-9]+$")).and(header("authorization","Bearer sk-or-restart"))
            .respond_with(move|request:&wiremock::Request|{
                let id=request.url.path().trim_start_matches("/batches/");
                let index=id.trim_start_matches("batch_").parse::<usize>().expect("job number")-1;
                if index==0&&!gate.load(Ordering::Acquire){
                    if epoch.load(Ordering::Acquire){reattached.notify_one();}else{notified.notify_one();}
                    return ResponseTemplate::new(200).set_body_json(json!({"id":id,"status":"in_progress"}));
                }
                let rows=rows.lock().expect("jobs");
                let row=&rows[index]["requests"][0];
                let (output,units)=if index==0 {
                    (json!({"type":"function_call","id":"effect-item","name":"exec_command","call_id":"approved-effect",
                        "arguments":json!({"cmd":"printf x >> approved-effect","workdir":cwd,"sandbox_permissions":"require_escalated","justification":"Exercise the approval and durable side effect in the mock."}).to_string(),"status":"completed"}),100_000)
                }else{
                    (json!({"type":"message","id":format!("message-{index}"),"role":"assistant","content":[{"type":"output_text","text":if index==1{"A summary preserving the approved tool receipt."}else{"Finished once."}}],"status":"completed"}),500)
                };
                ResponseTemplate::new(200).set_body_json(json!({"id":id,"status":"completed","usage":{"prompt_tokens":units,"completion_tokens":10,"total_tokens":units+10,"cost":0.01},"results":[{"custom_id":row["custom_id"],"response":{"status_code":200,"body":{"id":format!("response-{index}"),"status":"completed","output":[output]}}}]}))
            }).mount(provider).await;
    Mock::given(method("DELETE"))
        .and(path_regex("^/batches/batch_[0-9]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(provider)
        .await;
    let socket = home.path().join("control/server.sock");
    let capabilities = || InitializeCapabilities {
        experimental_api: true,
        required_human_approval: true,
        ..Default::default()
    };
    let mut first = spawn_server(home.path(), &socket)?;
    let mut client = connect_daemon_client(&socket, capabilities()).await?;
    let parent = request(&mut client, 2, "thread/start", json!({})).await?["thread"]["id"].clone();
    let setting = request(
        &mut client,
        3,
        "thread/settings/update",
        json!({"threadId":parent,"openrouterTransport":"batch"}),
    )
    .await?;
    assert_eq!(setting["openrouterTransport"], "batch");
    let child=request(&mut client,4,"thread/farm",json!({"threadId":parent,"requestId":uuid::Uuid::new_v4(),"mode":"fullHistory","input":[{"type":"text","text":"Run the approved job once."}]})).await?["childThreadId"].clone();
    timeout(READ_TIMEOUT, first_read.notified())
        .await
        .context("farm child did not poll its paid request")?;
    request_shutdown(&first, &socket).await?;
    wait_success(&mut first).await?;
    drop(client);
    let snapshot = codex_app_server_transport::daemon_recovery::read_snapshot(
        &codex_app_server_transport::daemon_recovery_file_path(home.path()),
    )?;
    let child_id = child.as_str().context("child id")?;
    assert!(snapshot.paid_children.contains(child_id), "{snapshot:?}");
    let turn = &snapshot.interrupted[child_id].turn_id;
    restored.store(true, Ordering::Release);
    let log = home.path().join("successor.log");
    let mut successor =
        spawn_server_with_stderr(home.path(), &socket, std::fs::File::create(&log)?.into())?;
    timeout(READ_TIMEOUT, restored_read.notified())
        .await
        .with_context(|| {
            format!(
                "did not reattach without a client: {}",
                std::fs::read_to_string(&log).unwrap_or_default()
            )
        })?;
    let mut client = connect_daemon_client(&socket, capabilities()).await?;
    request(&mut client, 5, "thread/resume", json!({"threadId":child})).await?;
    release.store(true, Ordering::Release);
    let mut approvals = 0;
    timeout(READ_TIMEOUT, async {
        loop {
            let Message::Text(raw) = client.next().await.context("successor disconnected")?? else {
                continue;
            };
            let msg: Value = serde_json::from_str(raw.as_str())?;
            match msg["method"].as_str() {
                Some("item/commandExecution/requestApproval") => {
                    assert_eq!(msg["params"]["threadId"], child);
                    approvals += 1;
                    client
                        .send(Message::Text(
                            json!({"id":msg["id"],"result":{"decision":"accept"}})
                                .to_string()
                                .into(),
                        ))
                        .await?;
                }
                Some("turn/completed") if msg["params"]["threadId"] == child => {
                    anyhow::ensure!(
                        msg["params"]["turn"]["status"] == "completed",
                        "child failed: {msg}"
                    );
                    assert_eq!(msg["params"]["turn"]["id"], *turn);
                    break Ok::<(), anyhow::Error>(());
                }
                Some("error") if msg["params"]["threadId"] == child => {
                    anyhow::bail!("child error: {msg}")
                }
                _ => {}
            }
        }
    })
    .await
    .context("recovered child did not finish")??;
    assert_eq!(approvals, 1);
    assert_eq!(
        std::fs::read_to_string(home.path().join("approved-effect"))?,
        "x"
    );
    {
        let rows = jobs.lock().expect("jobs");
        anyhow::ensure!(
            rows.len() >= 3,
            "missing compaction/continuation: {} rows",
            rows.len()
        );
        let bodies = rows
            .iter()
            .map(|row| &row["requests"][0]["body"])
            .collect::<Vec<_>>();
        assert!(
            bodies[1]["input"].to_string().contains("approved-effect"),
            "summary missing tool receipt"
        );
        assert!(
            bodies[2]["input"]
                .to_string()
                .contains("A summary preserving"),
            "continuation missing summary"
        );
        assert!(
            rows.iter()
                .all(|row| row["provider"]["only"] == json!(["google"]) && row["model"] == MODEL)
        );
    }
    request_shutdown(&successor, &socket).await?;
    wait_success(&mut successor).await?;
    Ok(())
}

#[tokio::test]
async fn openrouter_batch_stops_delivery_without_rebuying_uncertain_work() -> Result<()> {
    let home = TempDir::new()?;
    let provider = MockServer::start().await;
    Box::pin(mock_server::run(
        &[&provider],
        quarantine_spend(&home, &provider),
    ))
    .await
}

async fn quarantine_spend(home: &TempDir, provider: &MockServer) -> Result<()> {
    configure(home, provider, "google")?;
    mount_routed_catalog(provider, MODEL, "google").await;
    let submitted = Arc::new(Mutex::new(Vec::<Value>::new()));
    let effect = home.path().to_path_buf();
    let read = Arc::new(Notify::new());
    let release = Arc::new(AtomicBool::new(false));
    let deleted = Arc::new(Notify::new());
    let jobs = Arc::clone(&submitted);
    Mock::given(method("POST"))
        .and(path("/batches"))
        .and(header("authorization", "Bearer sk-or-restart"))
        .respond_with(move |r: &wiremock::Request| {
            let mut jobs = jobs.lock().expect("paid requests");
            jobs.push(r.body_json().expect("valid JSON"));
            if jobs.len() == 1 {
                ResponseTemplate::new(202).set_body_json(json!({"id":"batch_late"}))
            } else {
                // A lost acknowledgement can hide a charge. No second POST or live retry.
                ResponseTemplate::new(503)
            }
        })
        .expect(2)
        .mount(provider)
        .await;
    let jobs = Arc::clone(&submitted);
    let first = Arc::clone(&read);
    let released = Arc::clone(&release);
    Mock::given(method("GET")).and(path("/batches/batch_late"))
            .respond_with(move|_:&wiremock::Request|{
                if !released.load(Ordering::Acquire) {
                    first.notify_one();
                    return ResponseTemplate::new(200).set_body_json(json!({"id":"batch_late","status":"in_progress"}));
                }
                let rows=jobs.lock().expect("paid requests");
                let call=json!({"type":"function_call","id":"late-call","call_id":"late-effect","name":"exec_command","arguments":json!({"cmd":"touch late-effect","workdir":effect}).to_string(),"status":"completed"});
                ResponseTemplate::new(200).set_body_json(json!({"id":"batch_late","status":"completed","usage":{"prompt_tokens":30,"completion_tokens":10,"total_tokens":40,"cost":0.015},"results":[{"custom_id":rows[0]["requests"][0]["custom_id"],"response":{"status_code":200,"body":{"id":"late","status":"completed","output":[call]}}}]}))
            }).mount(provider).await;
    let cleaned = Arc::clone(&deleted);
    Mock::given(method("DELETE"))
        .and(path("/batches/batch_late"))
        .respond_with(move |_: &wiremock::Request| {
            cleaned.notify_one();
            ResponseTemplate::new(200).set_body_json(json!({}))
        })
        .expect(1)
        .mount(provider)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(provider)
        .await;
    let socket = home.path().join("control/server.sock");
    let caps = || InitializeCapabilities {
        experimental_api: true,
        ..Default::default()
    };
    let mut server = spawn_server(home.path(), &socket)?;
    let mut client = connect_daemon_client(&socket, caps()).await?;
    let id = request(&mut client, 2, "thread/start", json!({})).await?["thread"]["id"].clone();
    request(
        &mut client,
        3,
        "openrouter/model/route",
        json!({"slug":MODEL,"filter":{"only":["google/test"]}}),
    )
    .await?;
    let settings = json!({"threadId":id,"model":format!("{MODEL}:batch")});
    anyhow::ensure!(
        request(&mut client, 4, "thread/settings/update", settings.clone())
            .await
            .is_err(),
        "batch widened an individual endpoint to the whole host"
    );
    assert!(submitted.lock().expect("requests").is_empty());
    request(
        &mut client,
        5,
        "openrouter/model/route",
        json!({"slug":MODEL,"filter":{"only":["google"]}}),
    )
    .await?;
    let ack = request(&mut client, 6, "thread/settings/update", settings).await?;
    assert_eq!(ack["openrouterTransport"], "batch");
    let input = json!({"threadId":id,"input":[{"type":"text","text":"Execute once."}]});
    let turn = request(&mut client, 7, "turn/start", input.clone()).await?;
    timeout(READ_TIMEOUT, read.notified())
        .await
        .context("never polled the paid job")?;
    request(
        &mut client,
        8,
        "turn/interrupt",
        json!({"threadId":id,"turnId":turn["turn"]["id"]}),
    )
    .await?;
    release.store(true, Ordering::Release);
    timeout(READ_TIMEOUT, deleted.notified())
        .await
        .context("late result was not settled and deleted")?;
    assert!(!home.path().join("late-effect").exists());
    let dir = home
        .path()
        .join("openrouter-batches")
        .join(id.as_str().context("id")?);
    let receipts = std::fs::read_dir(dir)?
        .map(|e| Ok::<_, std::io::Error>(e?.path().join("generation.json")))
        .collect::<Result<Vec<_>, _>>()?;
    let [receipt] = receipts.as_slice() else {
        anyhow::bail!("expected the interrupted turn's single invoice");
    };
    let late: Value = serde_json::from_slice(&std::fs::read(receipt)?)?;
    assert_eq!(
        late["phase"]["Settled"]["settlement"]["usage"]["cost"],
        0.015
    );
    let unknown = request(&mut client, 9, "turn/start", input).await?;
    timeout(READ_TIMEOUT, async {
        loop {
            let Message::Text(raw) = client.next().await.context("disconnected")?? else {
                continue;
            };
            let msg: Value = serde_json::from_str(raw.as_str())?;
            if msg["method"] == "turn/completed"
                && msg["params"]["turn"]["id"] == unknown["turn"]["id"]
            {
                anyhow::ensure!(
                    msg["params"]["turn"]["status"] == "failed",
                    "uncertain submission did not fail: {msg}"
                );
                break Ok::<(), anyhow::Error>(());
            }
        }
    })
    .await??;
    request_shutdown(&server, &socket).await?;
    wait_success(&mut server).await?;
    let mut successor = spawn_server(home.path(), &socket)?;
    let mut client = connect_daemon_client(&socket, caps()).await?;
    request(&mut client, 10, "thread/resume", json!({"threadId":id})).await?;
    let status = request(
        &mut client,
        11,
        "openrouter/key/status",
        json!({"name":"mock"}),
    )
    .await?;
    assert_eq!(
        status["keys"][0]["outstandingBatches"], 1,
        "uncertain spend must not become available after restart"
    );
    assert_eq!(submitted.lock().expect("requests").len(), 2);
    assert!(!home.path().join("late-effect").exists());
    request_shutdown(&successor, &socket).await?;
    wait_success(&mut successor).await?;
    Ok(())
}

async fn finished_turn(
    client: &mut tokio_tungstenite::WebSocketStream<codex_uds::UnixStream>,
    id: &Value,
) -> Result<Value> {
    timeout(READ_TIMEOUT, async {
        loop {
            let Message::Text(raw) = client.next().await.context("server disconnected")?? else {
                continue;
            };
            let msg: Value = serde_json::from_str(raw.as_str())?;
            if msg["params"]["threadId"] == *id {
                if msg["method"] == "error" {
                    anyhow::bail!("turn failed: {msg}");
                }
                if msg["method"] == "turn/completed" {
                    let turn = msg["params"]["turn"].clone();
                    anyhow::ensure!(
                        turn["status"] == "completed",
                        "turn did not complete: {turn}"
                    );
                    return Ok(turn);
                }
            }
        }
    })
    .await?
}

#[tokio::test]
async fn openrouter_batch_compacts_tool_image_live_then_samples_in_batch() -> Result<()> {
    let home = TempDir::new()?;
    let provider = MockServer::start().await;
    Box::pin(mock_server::run(
        &[&provider],
        recover_sample(&home, &provider),
    ))
    .await
}

async fn recover_sample(home: &TempDir, provider: &MockServer) -> Result<()> {
    const PIXEL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    configure(home, provider, "together")?;
    mount_routed_catalog(provider, MODEL, "together").await;
    let live = Arc::new(Mutex::new(Vec::<Value>::new()));
    let paid = Arc::clone(&live);
    Mock::given(method("POST")).and(path("/responses")).and(header("authorization","Bearer sk-or-restart")).respond_with(move|r:&wiremock::Request|{
            paid.lock().expect("live requests").push(r.body_json().expect("JSON"));
            ResponseTemplate::new(200).set_body_json(json!({"id":"necessary-live","status":"completed","output":[{"type":"message","id":"summary","role":"assistant","content":[{"type":"output_text","text":"Summary of tool image."}],"status":"completed"}],"usage":{"input_tokens":400,"output_tokens":20,"total_tokens":420,"cost":0.024}}))
        }).expect(1).mount(provider).await;
    let batch = Arc::new(Mutex::new(Vec::<Value>::new()));
    let paid = Arc::clone(&batch);
    Mock::given(method("POST"))
        .and(path("/batches"))
        .and(header("authorization", "Bearer sk-or-restart"))
        .respond_with(move |r: &wiremock::Request| {
            let mut rows = paid.lock().expect("batches");
            rows.push(r.body_json().expect("JSON"));
            ResponseTemplate::new(202).set_body_json(json!({"id":format!("batch_{}",rows.len())}))
        })
        .expect(2)
        .mount(provider)
        .await;
    let rows = Arc::clone(&batch);
    let sample_waiting = Arc::new(Notify::new());
    let seen = Arc::clone(&sample_waiting);
    let release_sample = Arc::new(AtomicBool::new(false));
    let release = Arc::clone(&release_sample);
    Mock::given(method("GET")).and(path_regex("^/batches/batch_[12]$")).respond_with(move|r:&wiremock::Request|{
            let index=usize::from(r.url.path().ends_with('2'));
            if index==1 && !release.load(Ordering::Acquire) {
                seen.notify_one();
                return ResponseTemplate::new(200).set_body_json(json!({"id":"batch_2","status":"in_progress"}));
            }
            let rows=rows.lock().expect("batches");
            let id=format!("batch_{}",index+1);
            ResponseTemplate::new(200).set_body_json(json!({"id":id,"status":"completed","usage":{"prompt_tokens":90,"completion_tokens":3,"total_tokens":93,"cost":0.003},"results":[{"custom_id":rows[index]["requests"][0]["custom_id"],"response":{"status_code":200,"body":{"id":id,"status":"completed","output":[{"id":"answer","type":"message","role":"assistant","content":[{"type":"output_text","text":"Batch answer."}],"status":"completed"}]}}}]}))
        }).mount(provider).await;
    Mock::given(method("DELETE"))
        .and(path_regex("^/batches/batch_[12]$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(2)
        .mount(provider)
        .await;
    let socket = home.path().join("control/server.sock");
    let mut server = spawn_server(home.path(), &socket)?;
    let mut client = connect_daemon_client(
        &socket,
        InitializeCapabilities {
            experimental_api: true,
            ..Default::default()
        },
    )
    .await?;
    let thread = request(
        &mut client,
        2,
        "thread/start",
        json!({"model":format!("{MODEL}:batch")}),
    )
    .await?;
    let id = &thread["thread"]["id"];
    assert_eq!(thread["model"], MODEL);
    assert_eq!(thread["openrouterTransport"], "batch");
    request(&mut client,3,"thread/inject_items",json!({"threadId":id,"items":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"Inspect one pixel."},{"type":"input_image","image_url":PIXEL}]},
            {"type":"function_call","name":"view_image","arguments":"{}","call_id":"image-effect"},
            {"type":"function_call_output","call_id":"image-effect","output":[{"type":"input_image","image_url":PIXEL}]}
        ]})).await?;
    request(
        &mut client,
        4,
        "thread/compact/start",
        json!({"threadId":id}),
    )
    .await?;
    finished_turn(&mut client, id).await?;
    {
        let live = live.lock().expect("live");
        anyhow::ensure!(
            live.len() == 1,
            "compaction did not select the necessary live twin"
        );
        assert_eq!(live[0]["model"], MODEL);
        assert_eq!(
            live[0]["provider"],
            json!({"only":["together"],"allow_fallbacks":false})
        );
        assert!(live[0]["input"].to_string().contains("data:image/"));
    }
    let root = home
        .path()
        .join("openrouter-batches")
        .join(id.as_str().context("thread id")?);
    let journal = std::fs::read_dir(root)?
        .next()
        .context("compaction journal missing")??
        .path()
        .join("compaction.json");
    let compact: Value = serde_json::from_slice(&std::fs::read(journal)?)?;
    assert_eq!(
        compact["history"][0]["job"]["Live"],
        "CompactionNeedsInlineImage"
    );
    assert_eq!(compact["history"][0]["usage"]["cost"], 0.024);
    request(
        &mut client,
        5,
        "turn/start",
        json!({"threadId":id,"input":[{"type":"text","text":"Continue on batch."}]}),
    )
    .await?;
    let turn = finished_turn(&mut client, id).await?;
    let params = json!({"threadId":id,"expectedTurnId":turn["id"],"prompt":"Sample privately."});
    assert!(
        request(&mut client, 6, "thread/sample", params.clone())
            .await
            .is_err()
    );
    let sample = request(&mut client, 7, "thread/sample/start", params).await?;
    timeout(READ_TIMEOUT, sample_waiting.notified())
        .await
        .context("sample was not admitted")?;
    let once = json!({"threadId":id,"handle":sample["handle"],"waitMs":0});
    assert_eq!(
        request(&mut client, 8, "thread/sample/read", once).await?["status"],
        "pending"
    );
    // There is no graceful daemon handoff for this fork. Recover only its
    // paid job through the persistent handle, never its tool executor.
    server.kill().await?;
    drop(client);
    let mut successor = spawn_server(home.path(), &socket)?;
    let mut client = connect_daemon_client(
        &socket,
        InitializeCapabilities {
            experimental_api: true,
            ..Default::default()
        },
    )
    .await?;
    let resumed = request(&mut client, 2, "thread/resume", json!({"threadId":id})).await?;
    assert_eq!(resumed["model"], MODEL);
    assert_eq!(resumed["openrouterTransport"], "batch");
    release_sample.store(true, Ordering::Release);
    let read = json!({"threadId":id,"handle":sample["handle"],"waitMs":10000});
    let sampled = request(&mut client, 8, "thread/sample/read", read.clone()).await?;
    assert_eq!(sampled["status"], "ready", "{sampled}");
    assert_eq!(sampled["result"]["text"], "Batch answer.");
    assert_eq!(
        sampled,
        request(&mut client, 9, "thread/sample/read", read).await?
    );
    request_shutdown(&successor, &socket).await?;
    wait_success(&mut successor).await?;
    Ok(())
}
