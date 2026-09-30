//! Native user policy cannot override enterprise admission, including retired
//! transport identity constraints. Exercise the existing public RPC fixture.

use super::*;
use crate::common::TestAppServer;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use std::collections::HashMap;

async fn user_owned_mcp(
    State(gate): State<Arc<ModelGate>>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    Json(body): Json<Value>,
) -> Response {
    gate.legacy_connections.fetch_add(1, Ordering::SeqCst);
    gate.changed.notify_one();
    let result = match body["method"].as_str() {
        Some("initialize") => json!({
            "protocolVersion": body["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "user-owned", "version": "1"}
        }),
        Some("tools/list") => {
            json!({"tools": ["list_threads", "unrelated_probe"].map(|name| json!({
                "name": name, "description": format!("Unrelated user server {}", uri.path()),
                "inputSchema": {"type": "object", "properties": {}},
                "annotations": {"readOnlyHint": true}
            }))})
        }
        Some("tools/call") => {
            let name = body["params"]["name"].as_str().expect("raw MCP tool name");
            let server = if uri.path() == "/prefixed-mcp" {
                "mcp__codex_tui"
            } else {
                "codex_tui"
            };
            gate.legacy_calls
                .lock()
                .await
                .push(format!("{server}/{name}"));
            gate.changed.notify_one();
            json!({"content": [{"type": "text", "text": format!("user-owned:{server}/{name}")}], "isError": false})
        }
        _ => return Response::builder().status(202).body(Body::empty()).unwrap(),
    };
    Response::builder()
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": body["id"], "result": result}).to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn native_approval_requires_managed_admission_before_retiring_a_legacy_identity() -> Result<()>
{
    for case in [
        "excluded",
        "missing_identity",
        "mismatched_identity",
        "admitted_identity",
        "user_owned_identity",
    ] {
        let gate = Arc::new(ModelGate::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let endpoint = format!("http://{address}/mcp");
        let mcp_route = if case == "user_owned_identity" {
            post(user_owned_mcp)
        } else {
            any(retired_frontend_transport)
        };
        let model_server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/responses", post(model_response))
                    .route("/mcp", mcp_route)
                    .route("/prefixed-mcp", post(user_owned_mcp))
                    .with_state(Arc::clone(&gate)),
            )
            .into_future(),
        );
        let home = TempDir::new()?;
        create_config_toml(home.path(), &format!("http://{address}"), "never")?;
        let path = home.path().join("config.toml");
        let mut config = std::fs::read_to_string(&path)?;
        config.push_str(
            "\n[tools.thread_coordination.tools.create_thread]\napproval_mode = \"approve\"\n",
        );
        if case == "user_owned_identity" {
            // Even the old listener's URL/header shape is insufficient to claim
            // ownership of a durable user-configured server.
            config.push_str(&format!("\n[mcp_servers.codex_tui]\nurl = {endpoint:?}\nrequired = true\ndefault_tools_approval_mode = 'approve'\nhttp_headers = {{Authorization = 'Bearer {}'}}\n[mcp_servers.mcp__codex_tui]\nurl = {:?}\nrequired = true\ndefault_tools_approval_mode = 'approve'\ndisabled_tools = ['unrelated_probe']\n", uuid::Uuid::new_v4(), format!("http://{address}/prefixed-mcp")));
        }
        std::fs::write(path, config)?;
        let requirements = match case {
            "excluded" => "[mcp_servers]\n".into(),
            "admitted_identity" | "user_owned_identity" => {
                let mut requirements =
                    format!("[mcp_servers.codex_tui.identity]\nurl = {endpoint:?}\n");
                if case == "user_owned_identity" {
                    requirements.push_str(&format!(
                        "[mcp_servers.mcp__codex_tui.identity]\nurl = {:?}\n",
                        format!("http://{address}/prefixed-mcp")
                    ));
                }
                requirements
            }
            "missing_identity" | "mismatched_identity" => {
                "[mcp_servers.codex_tui.identity]\nurl = 'https://enterprise.invalid/mcp'\n".into()
            }
            _ => unreachable!(),
        };
        std::fs::write(home.path().join("requirements.toml"), requirements)?;
        let mut server = TestAppServer::builder()
            .with_codex_home(home.path())
            .build_initialized()
            .await?;
        let source = server
            .start_thread(ThreadStartParams {
                config: matches!(case, "mismatched_identity" | "admitted_identity").then(|| {
                    let mut legacy = frontend_transport_config(&endpoint);
                    legacy["enabled"] = json!(false);
                    HashMap::from([("mcp_servers.codex_tui".into(), legacy)])
                }),
                ..Default::default()
            })
            .await?;
        *gate.tools.lock().await = vec![responses::ev_function_call_with_namespace(
            "managed-create",
            "mcp__codex_tui",
            "create_thread",
            &json!({"prompt": "Start only if managed admission grants it"}).to_string(),
        )];
        if case == "user_owned_identity" {
            gate.tools
                .lock()
                .await
                .push(responses::ev_function_call_with_namespace(
                    "native-list",
                    "mcp__codex_tui",
                    "list_threads",
                    "{}",
                ));
        }
        // Discover actual finalized names, including collision hashes, rather
        // than duplicating the normalizer in a fixture or prescribing aliases.
        let discovery = if case == "user_owned_identity" {
            let gate = Arc::clone(&gate);
            Some(tokio::spawn(async move {
                let result = async {
                    timeout(TIMEOUT, gate.entered.notified()).await?;
                    let calls = gate.calls.lock().await.clone();
                    let tools = calls[0]["tools"].as_array().context("advertised tools")?;
                    let mut namespaces = Vec::new();
                    for (server, route, names) in [
                        (
                            "codex_tui",
                            "/mcp",
                            &["list_threads", "unrelated_probe"][..],
                        ),
                        ("mcp__codex_tui", "/prefixed-mcp", &["list_threads"][..]),
                    ] {
                        let marker = format!("Unrelated user server {route}");
                        let spec = tools
                            .iter()
                            .find(|tool| {
                                tool["tools"].as_array().is_some_and(|children| {
                                    children.iter().any(|child| {
                                        child["description"].as_str().is_some_and(|description| {
                                            description.contains(&marker)
                                        })
                                    })
                                })
                            })
                            .context("retained raw server's advertised namespace")?;
                        let namespace = spec["name"].as_str().context("canonical namespace")?;
                        anyhow::ensure!(
                            namespace.starts_with("mcp__mcp__codex_tui_"),
                            "unexpected canonical namespace: {namespace}"
                        );
                        namespaces.push(namespace);
                        let advertised: Vec<_> = spec["tools"]
                            .as_array()
                            .context("raw server tools")?
                            .iter()
                            .filter_map(|tool| tool["name"].as_str())
                            .collect();
                        anyhow::ensure!(
                            advertised == names,
                            "raw policy must remain authoritative: {server}: {advertised:?}"
                        );
                        gate.tools.lock().await.extend(names.iter().map(|name| {
                            responses::ev_function_call_with_namespace(
                                &format!("{server}/{name}"),
                                namespace,
                                name,
                                "{}",
                            )
                        }));
                    }
                    anyhow::ensure!(
                        namespaces[0] != namespaces[1],
                        "raw registrations must not shadow each other"
                    );
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                gate.release.notify_one();
                result
            }))
        } else {
            gate.release.notify_one();
            None
        };
        server
            .start_turn_and_wait_for_completion(TurnStartParams {
                thread_id: source.thread.id,
                input: vec![UserInput::Text {
                    text: "coordinate".into(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            })
            .await?;
        if let Some(discovery) = discovery {
            discovery.await??;
        }
        let calls = gate.calls.lock().await.clone();
        let output = calls
            .iter()
            .flat_map(|call| call["input"].as_array().into_iter().flatten())
            .find(|item| {
                item["call_id"] == "managed-create" && item["type"] == "function_call_output"
            })
            .context("coordination authorization result")?["output"]
            .as_str()
            .context("tool output text")?;
        if matches!(case, "admitted_identity" | "user_owned_identity") {
            timeout(TIMEOUT, async {
                loop {
                    if gate.calls.lock().await.len() >= 3 {
                        break;
                    }
                    gate.changed.notified().await;
                }
            })
            .await?;
            let calls = gate.calls.lock().await.clone();
            assert_eq!(calls.len(), 3);
            assert!(serde_json::from_str::<Value>(output)?["threadId"].is_string());
        } else {
            assert_eq!(
                calls.len(),
                2,
                "managed denial must not create a destination: {case}"
            );
            assert!(
                output.contains("native coordination is denied"),
                "{case}: {output}"
            );
        }
        if case == "user_owned_identity" {
            timeout(TIMEOUT, async {
                while gate.legacy_connections.load(Ordering::SeqCst) == 0 {
                    gate.changed.notified().await;
                }
            })
            .await
            .context("an unrelated user server was suppressed")?;
            let mut raw_calls = gate.legacy_calls.lock().await.clone();
            raw_calls.sort();
            assert_eq!(
                raw_calls,
                [
                    "codex_tui/list_threads",
                    "codex_tui/unrelated_probe",
                    "mcp__codex_tui/list_threads"
                ]
            );
            let native = calls
                .iter()
                .flat_map(|call| call["input"].as_array().into_iter().flatten())
                .find(|item| {
                    item["call_id"] == "native-list" && item["type"] == "function_call_output"
                })
                .context("historical callable spelling must dispatch native")?["output"]
                .as_str()
                .context("native list output")?;
            assert_eq!(serde_json::from_str::<Value>(native)?["schemaVersion"], 4);
            for name in raw_calls {
                assert!(
                    calls
                        .iter()
                        .flat_map(|call| call["input"].as_array().into_iter().flatten())
                        .any(|item| {
                            item["call_id"] == name
                                && item["output"].as_str().is_some_and(|output| {
                                    output.contains(&format!("user-owned:{name}"))
                                })
                        }),
                    "canonical MCP dispatch lost {name}"
                );
            }
        } else {
            assert_eq!(gate.legacy_connections.load(Ordering::SeqCst), 0, "{case}");
        }
        server.shutdown().await?;
        model_server.abort();
    }
    Ok(())
}
