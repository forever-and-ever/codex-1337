//! Catalog resource-tool text reaches direct calls and Code Mode without changing MCP execution.

use super::super::rmcp_client::remote_aware_environment_id;
use super::super::rmcp_client::remote_aware_stdio_server_bin;
use anyhow::Result;
use codex_core::ConfigRefreshOutcome;
use codex_features::Feature;
use codex_mcp::McpServerKey;
use codex_protocol::mcp::McpServerConnectionStatus;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::McpStartupStatus;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use core_test_support::context_snapshot;
use core_test_support::context_snapshot::ContextSnapshotOptions;
use core_test_support::mock_server;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event_match;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;
use test_case::test_case;

#[test_case(ToolMode::Direct; "direct")]
#[test_case(ToolMode::CodeModeOnly; "code_mode")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_resource_messages(tool_mode: ToolMode) -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_wine_exec!(Ok(()), "requires a Windows test_stdio_server binary");
    let server = responses::start_mock_server().await;
    mock_server::run(&[&server], async {
    let command = remote_aware_stdio_server_bin()?;
    let calls = [
        ("list_mcp_resources", r#"{"server":"resources"}"#),
        ("list_mcp_resource_templates", r#"{"server":"resources"}"#),
        (
            "read_mcp_resource",
            r#"{"server":"resources","uri":"memo://codex/example-note"}"#,
        ),
    ];
    let mut messages = serde_json::Map::new();
    for (name, _) in calls {
        let mut parameters = json!({
            "type": "object",
            "properties": {"server": {"type": "string", "description": format!("Catalog server for {name}.")}},
            "additionalProperties": false,
        });
        if name == "read_mcp_resource" {
            parameters["properties"]["uri"] = json!({"type": "string", "description": "Catalog URI returned by resource discovery."});
            parameters["required"] = json!(["server", "uri"]);
        } else {
            parameters["properties"]["cursor"] =
                json!({"type": "string", "description": "Catalog cursor from the previous page."});
        }
        messages.insert(
            name.to_string(),
            json!({
                "description": format!("Catalog instructions for {name}."),
                "parameters": parameters.to_string(),
            }),
        );
    }
    let catalog = serde_json::from_value(json!(messages))?;
    let mut test = test_codex()
        .with_config(move |config| {
            super::configure_scenario_catalog(config);
            config.code_mode.disable_in_process_fallback = true;
            config
                .features
                .enable(Feature::CodeModeHost)
                .expect("enable Code Mode host");
            config
                .mcp_servers
                .set(HashMap::from([(
                    "resources".to_string(),
                    serde_json::from_value(json!({
                        "command": command,
                        "environment_id": remote_aware_environment_id(),
                        "cwd": config.cwd,
                    }))
                    .expect("MCP resource fixture"),
                ), (
                    "known_resources".to_string(),
                    serde_json::from_value(json!({
                        "command": command,
                        "environment_id": remote_aware_environment_id(),
                        "cwd": config.cwd,
                    }))
                    .expect("known resource capability fixture"),
                )]))
                .expect("configure resources server");
        })
        .with_model_info_override("gpt-6-astra", move |model| {
            model.tool_mode = Some(tool_mode);
            model.use_responses_lite = false;
            let model_messages = model.model_messages.as_mut().expect("model messages");
            model_messages
                .tools
                .get_or_insert_with(Default::default)
                .mcp_resources = Some(catalog);
        })
        .build_with_auto_env(&server)
        .await?;
    wait_for_mcp_server(&test.codex, "resources").await?;
    let mut options = test.start_thread_options().await;
    options.session_source = Some(SessionSource::SubAgent(SubAgentSource::Other(
        "resource-discovery".to_string(),
    )));
    test.codex = test.thread_manager.start_thread(options).await?.thread;
    // Fork resource helpers require a known resource-capable connection. Admit one
    // explicitly while leaving the discovery target dormant in this child.
    test.codex.call_mcp_tool(
        "known_resources",
        /*environment_id*/ None,
        "echo",
        Some(json!({"message": "discover the resource capability"})),
        None,
    ).await?;
    let resource_calls = |id: &str, calls: &[(&str, &str)]| {
        let mut events = vec![responses::ev_response_created(id)];
        if tool_mode == ToolMode::CodeModeOnly {
            let script = calls
                .iter()
                .map(|(name, args)| format!("text(describe_tool(\"{name}\"));\ntext(await tools.{name}({args}));"))
                .collect::<Vec<_>>()
                .join("\n");
            events.push(responses::ev_custom_tool_call(id, "exec", &script));
        } else {
            events.extend(calls.iter().map(|(name, args)| {
                responses::ev_function_call(&format!("{id}-{name}"), name, args)
            }));
        }
        events.push(responses::ev_completed(id));
        responses::sse(events)
    };
    let broad = [
        ("list_mcp_resources", "{}"),
        ("list_mcp_resource_templates", "{}"),
    ];
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            resource_calls("broad", &broad),
            responses::sse_completed("broad-done"),
            resource_calls("resources-call", &calls),
            responses::sse_completed("done"),
        ],
    )
    .await;
    test.submit_turn("List available MCP resources and templates.")
        .await?;
    let (config, _) = test.codex.current_mcp_config_and_runtime_context().await;
    assert_eq!(
        test.codex
            .mcp_connection_statuses(&config)
            .await
            .get(&McpServerKey::unscoped("resources")),
        Some(&McpServerConnectionStatus::NotStarted)
    );
    test.submit_turn("Query the deferred server by name and read its example note.")
        .await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    if tool_mode == ToolMode::Direct {
        for (tool, key) in [
            ("list_mcp_resources", "resources"),
            ("list_mcp_resource_templates", "resourceTemplates"),
        ] {
            let output = requests[1]
                .function_call_output_text(&format!("broad-{tool}"))
                .expect("aggregate result");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&output)?,
                json!({
                    key: [if key == "resources" {
                        json!({
                            "server": "known_resources",
                            "uri": "memo://codex/example-note",
                            "name": "example-note",
                            "title": "Example Note",
                            "description": "A sample MCP resource exposed for integration tests.",
                            "mimeType": "text/plain",
                        })
                    } else {
                        json!({
                            "server": "known_resources",
                            "uriTemplate": "memo://codex/{slug}",
                            "name": "codex-memo",
                            "title": "Codex Memo",
                            "description": "Template for memo://codex/{slug} resources used in tests.",
                            "mimeType": "text/plain",
                        })
                    }],
                    "deferredServers": ["resources"],
                })
            );
        }
    }
    let body = requests[0].body_json();
    for (name, message) in &messages {
        if tool_mode == ToolMode::CodeModeOnly {
            // The roster glosses each tool with its catalog description; the contract, with its
            // catalog parameter guidance, arrives through `describe_tool`.
            assert!(requests[0].body_contains_text(&format!(
                "- tools.{name}: {}",
                message["description"].as_str().expect("description")
            )));
            let contract_request = if name == "read_mcp_resource" { &requests[3] } else { &requests[1] };
            assert!(contract_request.body_contains_text(&format!("Catalog server for {name}.")));
        } else {
            let tool = body["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .find(|tool| tool["name"] == *name)
                .expect("resource tool");
            assert_eq!(
                tool,
                &json!({
                    "type": "function", "name": name, "description": message["description"],
                    "parameters": serde_json::from_str::<serde_json::Value>(message["parameters"].as_str().expect("parameters"))?,
                    "strict": false,
                })
            );
        }
    }
    assert!(requests[3].body_contains_text("Example Note"));
    assert!(requests[3].body_contains_text("memo://codex/{slug}"));
    assert!(
        requests[3]
            .body_contains_text("This is a sample MCP resource served by the rmcp test server.")
    );
    if tool_mode == ToolMode::CodeModeOnly {
        insta::assert_snapshot!(
            "mcp_resource_messages",
            context_snapshot::format_request_history_snapshot(
                "A known resource-capable connection enables resource helpers. Broad discovery retains that connection's resources and names a second dormant server; calling the dormant server by name returns its resources and templates through Code Mode.",
                &requests,
                &ContextSnapshotOptions::default().include_request_settings(),
            )
        );
    }
    Ok(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_helpers_across_server_changes() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_wine_exec!(Ok(()), "requires a Windows test_stdio_server binary");
    let server = responses::start_mock_server().await;
    mock_server::run(&[&server], async {
    let command = remote_aware_stdio_server_bin()?;
    let test = test_codex()
        .with_config(move |config| {
            super::configure_scenario_catalog(config);
            config.code_mode.disable_in_process_fallback = true;
            config.features.enable(Feature::CodeModeHost).unwrap();
        })
        .with_model_info_override("gpt-6-astra", |model| {
            model.tool_mode = Some(ToolMode::CodeModeOnly);
            model.use_responses_lite = false;
        })
        .build_with_auto_env(&server)
        .await?;
    let script = r#"text(["list_mcp_resources", "list_mcp_resource_templates", "read_mcp_resource"].map(name => typeof tools[name]).join(","));
    if (typeof tools.list_mcp_resources === "function") {
      text(await tools.list_mcp_resources({}));
      text(await tools.list_mcp_resource_templates({}));
      try {
        await tools.read_mcp_resource({ server: "missing", uri: "memo://missing" });
      } catch (error) {
        text(String(error));
      }
    }"#;
    let mut events = Vec::new();
    for phase in ["before", "registered", "removed"] {
        events.push(responses::sse(vec![
            responses::ev_custom_tool_call(phase, "exec", script),
            responses::ev_completed(phase),
        ]));
        events.push(responses::sse_completed(&format!("{phase}-done")));
    }
    let mock = responses::mount_sse_sequence(&server, events).await;
    test.submit_turn("Inspect resources without any configured server.")
        .await?;
    let current = test.codex.config().await;
    let mut next = current.as_ref().clone();
    next.mcp_servers.set(HashMap::from([(
        "resources".to_string(),
        serde_json::from_value(json!({
            "command": command,
            "environment_id": remote_aware_environment_id(),
            "cwd": current.cwd,
        }))?,
    )]))?;
    assert_eq!(
        test.codex.refresh_mcp_config(current, next).await,
        ConfigRefreshOutcome::Published,
    );
    let startup = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::McpStartupUpdate(update)
            if update.server == "resources"
                && !matches!(update.status, McpStartupStatus::Starting) =>
        {
            Some(update.status.clone())
        }
        _ => None,
    })
    .await;
    assert!(matches!(startup, McpStartupStatus::Ready), "{startup:?}");
    test.submit_turn("Inspect resources with one server.")
        .await?;
    let current = test.codex.config().await;
    let mut next = current.as_ref().clone();
    next.mcp_servers.set(HashMap::new())?;
    assert_eq!(
        test.codex.refresh_mcp_config(current, next).await,
        ConfigRefreshOutcome::Published,
    );
    test.submit_turn("Inspect resources after removing the last server.")
        .await?;
    let requests = mock.requests();
    let tools = requests
        .iter()
        .map(|request| request.body_json()["tools"].clone())
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 6);
    // The declarations never move: roster amendments carry the resource helpers in and out.
    assert_eq!(tools[0], tools[2]);
    assert_eq!(tools[0], tools[4]);
    let roster = core_test_support::nested_tools::fragments(&requests[4].input());
    let [_, registered, removed] = roster.as_slice() else {
        panic!("each server change amends the roster once: {roster:?}")
    };
    assert!(
        registered.starts_with("<nested_tools>\nAdded or changed:\n")
            && registered.contains("\n- tools.list_mcp_resources: "),
        "{registered}"
    );
    assert!(
        removed.starts_with("<nested_tools>\nRemoved:\n")
            && removed.contains("\n- tools.list_mcp_resources\n"),
        "{removed}"
    );
    for (request, phase) in
        requests
            .iter()
            .skip(1)
            .step_by(2)
            .zip(["before", "registered", "removed"])
    {
        let output = request.custom_tool_call_output(phase).to_string();
        if phase == "registered" {
            assert!(output.contains("function,function,function"), "{output}");
            assert!(output.contains("Example Note"), "{output}");
            assert!(output.contains("memo://codex/{slug}"), "{output}");
            assert!(output.contains("unknown MCP server 'missing'"), "{output}");
        } else {
            assert!(output.contains("undefined,undefined,undefined"), "{output}");
        }
    }
    test.codex.shutdown_and_wait().await?;
    Ok(())
    })
    .await
}
