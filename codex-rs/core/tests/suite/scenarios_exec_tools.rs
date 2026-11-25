//! Catalogue changes amend the nested-tool roster; frozen exec sections refresh only at
//! compaction.

use anyhow::Result;
use codex_features::Feature;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use core_test_support::context_snapshot;
use core_test_support::context_snapshot::ContextSnapshotOptions;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_mcp_server;
use core_test_support::wait_for_successful_turn;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;

use super::super::rmcp_client::remote_aware_environment_id;
use super::super::rmcp_client::remote_aware_stdio_server_bin;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_sections_and_entries_append_without_repeating_the_catalog() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_wine_exec!(Ok(()), "requires a Windows test_stdio_server fixture");
    let server = responses::start_mock_server().await;
    let command = remote_aware_stdio_server_bin()?;
    let environment_id = remote_aware_environment_id();
    let server_config = |tools: &[&str], instructions: &str, omitted: &[&str]| {
        serde_json::from_value(json!({
            "command": command,
            "environment_id": environment_id,
            "env": {"MCP_TEST_SERVER_INSTRUCTIONS": instructions},
            "enabled_tools": tools,
            "omit_tools_from": omitted,
            "default_tools_approval_mode": "approve",
        }))
    };
    let original_instructions = "Echo service instructions.";
    let updated_instructions = "Echo service instructions. Durations are milliseconds.";
    let initial_server = server_config(&["echo"], original_instructions, &["deferred"])?;
    let mut builder = test_codex().with_config(move |config| {
        super::configure_scenario_catalog(config);
        for feature in [
            Feature::IncrementalTools,
            Feature::IncrementalExecTools,
            Feature::DeferredToolWorldState,
        ] {
            config
                .features
                .enable(feature)
                .expect("enable catalog updates");
        }
        config
            .features
            .disable(Feature::CodeModeOnlyStrictThirdPartyTools)
            .expect("allow eager MCP tools");
        config.code_mode.disable_in_process_fallback = true;
        config
            .mcp_servers
            .set(HashMap::from([(
                "echo_service".to_string(),
                initial_server,
            )]))
            .expect("set MCP fixture");
    });
    builder = builder.with_model_info_override("catalog-initial", |model| {
        model.use_responses_lite = true;
        model.tool_mode = Some(ToolMode::CodeModeOnly);
        model.supports_search_tool = true;
    });
    for (slug, description, deferred_guidance, shared_types) in [
        (
            "catalog-updated",
            "Run JavaScript. Await every tool call before returning.",
            "Find deferred tools in ALL_TOOLS.",
            "type CallToolResult<T = unknown> = { content: unknown[]; structuredContent?: T; isError?: boolean };",
        ),
        (
            "catalog-compacted",
            "Run JavaScript. Await every tool call and inspect its result before returning.",
            "Find deferred tools by name in ALL_TOOLS.",
            "type CallToolResult<T = unknown> = { content: unknown[]; structuredContent?: T; isError?: boolean; _meta?: unknown };",
        ),
    ] {
        builder = builder.with_model_info_override(slug, move |model| {
            model.use_responses_lite = true;
            model.tool_mode = Some(ToolMode::CodeModeOnly);
            model.supports_search_tool = true;
            model.model_messages.as_mut().expect("model messages").tools = Some(
                serde_json::from_value(json!({
                    "code_mode": {
                        "exec": {"description": description},
                        "deferred_nested_tools_guidance": deferred_guidance,
                        "mcp_typescript_preamble": shared_types,
                    },
                    "mcp_resources": {"read_mcp_resource": {
                        "description": "Read a selected resource revision.",
                        "parameters": json!({
                            "type": "object",
                            "properties": {"server": {"type": "string"}, "uri": {"type": "string"}, "revision": {"type": "integer"}},
                            "required": ["server", "uri"],
                            "additionalProperties": false,
                        }).to_string(),
                    }},
                })).expect("updated tool messages"),
            );
        });
    }
    let test = builder
        .with_model("catalog-initial")
        .build_with_auto_env(&server)
        .await?;
    wait_for_mcp_server(&test.codex, "echo_service").await?;
    let mut events = (0..12)
        .map(|index| responses::sse_completed(&format!("response-{index}")))
        .collect::<Vec<_>>();
    events.push(responses::sse(vec![
        json!({"type": "response.output_item.done", "item": {
            "type": "compaction", "encrypted_content": "CATALOG_CHECKPOINT",
        }}),
        responses::ev_completed("response-12"),
    ]));
    events.push(responses::sse_completed("response-13"));
    let mock = responses::mount_sse_sequence(&server, events).await;
    test.submit_text_turn("Initial eager catalog: echo.")
        .await?;
    // Refresh through the public MCP configuration and call boundaries, as production does.
    let ready = Some(json!({"message": "catalog ready"}));
    for (tools, instructions, prompt) in [
        (
            vec!["echo", "cwd"],
            original_instructions,
            "Nested tool addition: cwd.",
        ),
        (
            vec!["echo", "cwd"],
            updated_instructions,
            "Namespace instructions redefinition: durations.",
        ),
        (
            vec!["echo"],
            updated_instructions,
            "Nested tool removal: cwd.",
        ),
    ] {
        let current = test.codex.config().await;
        let mut updated = current.as_ref().clone();
        updated.mcp_servers.set(HashMap::from([(
            "echo_service".to_string(),
            server_config(&tools, instructions, &["deferred"])?,
        )]))?;
        let _ = test.codex.refresh_mcp_config(current, updated).await;
        test.codex
            .call_mcp_tool(
                "echo_service",
                /*environment_id*/ None,
                "echo",
                ready.clone(),
                /*meta*/ None,
            )
            .await?;
        test.submit_text_turn(prompt).await?;
    }
    // Reapplying the current model changes nothing.
    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model: Some("catalog-initial".to_string()),
            ..Default::default()
        },
    )
    .await?;
    test.submit_text_turn("Unchanged catalog: emit no update.")
        .await?;

    for (tools, instructions, omitted, prompt) in [
        (
            vec!["echo"],
            "",
            vec!["deferred"],
            "Namespace instructions removal: keep echo.",
        ),
        (vec!["echo"], "", vec![], "Eager to deferred: echo."),
        (
            vec!["echo"],
            updated_instructions,
            vec!["deferred"],
            "Deferred to eager: echo.",
        ),
        (vec![], "", vec![], "Remove all echo service entries."),
        (
            vec!["echo", "cwd"],
            updated_instructions,
            vec!["deferred"],
            "Re-enable echo service entries: echo and cwd.",
        ),
    ] {
        let current = test.codex.config().await;
        let mut updated = current.as_ref().clone();
        updated.mcp_servers.set(if tools.is_empty() {
            HashMap::new()
        } else {
            HashMap::from([(
                "echo_service".to_string(),
                server_config(&tools, instructions, &omitted)?,
            )])
        })?;
        let _ = test.codex.refresh_mcp_config(current, updated).await;
        let result = test
            .codex
            .call_mcp_tool(
                "echo_service",
                /*environment_id*/ None,
                "echo",
                ready.clone(),
                /*meta*/ None,
            )
            .await;
        if tools.is_empty() {
            assert!(result.is_err(), "removed tools must not remain callable");
        } else {
            result?;
        }
        test.submit_text_turn(prompt).await?;
    }
    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model: Some("catalog-updated".to_string()),
            ..Default::default()
        },
    )
    .await?;
    test.submit_text_turn(
        "Combined redefinition: resource revision, common instructions, and shared types.",
    )
    .await?;
    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model: Some("catalog-compacted".to_string()),
            ..Default::default()
        },
    )
    .await?;
    test.submit_text_turn("Only common instructions and shared types change: wait for compaction.")
        .await?;
    test.codex.submit(Op::Compact).await?;
    wait_for_successful_turn(&test.codex).await;
    test.submit_text_turn("After compaction: restore the full current catalog.")
        .await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 14);
    let mut previous = Vec::new();
    let mut declarations = Vec::new();
    let mut amendments = Vec::new();
    // Snapshots normalize IDs and metadata, so check the retained raw prefix separately.
    for (index, request) in requests.iter().enumerate() {
        let input = request.input();
        let (appended, roster) = match index {
            // Compaction sees a disposable canonical-instruction view, whose rewritten harness
            // instructions have fresh request IDs. Its semantic prefix, roster included, is
            // intact.
            12 => {
                let without_ids = |items: &[serde_json::Value]| {
                    responses::strip_response_item_ids_from_json(serde_json::Value::Array(
                        items.to_vec(),
                    ))
                };
                assert_eq!(
                    without_ids(&input[..previous.len()]),
                    without_ids(&previous),
                    "compaction changed the semantic retained prefix",
                );
                let appended = &input[previous.len()..];
                (appended, core_test_support::nested_tools::fragments(appended))
            }
            13 => (
                input.as_slice(),
                core_test_support::nested_tools::fragments(&input),
            ),
            _ => {
                assert_eq!(
                    &input[..previous.len()],
                    previous.as_slice(),
                    "retained context changed at request {index}"
                );
                let appended = &input[previous.len()..];
                (appended, core_test_support::nested_tools::fragments(appended))
            }
        };
        declarations.push(
            appended
                .iter()
                .filter(|item| item["type"] == "additional_tools")
                .filter_map(|item| responses::namespace_child_tool(item, "functions", "exec"))
                .cloned()
                .collect::<Vec<_>>(),
        );
        amendments.push(roster);
        previous = input;
    }
    // A window declares `exec` once: no catalogue change moves the declaration.
    assert_eq!(
        declarations.iter().map(Vec::len).collect::<Vec<_>>(),
        [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
    let cwd = "- mcp__echo_service.cwd (tools.mcp__echo_service__cwd)";
    let echo = "- mcp__echo_service.echo (tools.mcp__echo_service__echo)";
    let revised = "- tools.read_mcp_resource: Read a selected resource revision.";
    assert_eq!(
        amendments[1],
        [format!(
            "<nested_tools>\nAdded or changed:\n{cwd}: Return the current working directory of this test server process.\n</nested_tools>"
        )]
    );
    assert_eq!(
        amendments[3],
        [format!("<nested_tools>\nRemoved:\n{cwd}\n</nested_tools>")]
    );
    assert_eq!(
        amendments[8],
        [format!(
            "<nested_tools>\nRemoved:\n{echo}\n- tools.list_mcp_resource_templates\n- tools.list_mcp_resources\n- tools.read_mcp_resource\n</nested_tools>"
        )]
    );
    let [reenabled] = amendments[9].as_slice() else {
        panic!("re-enabling amends the roster once: {:?}", amendments[9])
    };
    assert!(reenabled.starts_with(&format!("<nested_tools>\nAdded or changed:\n{cwd}: ")));
    for entry in [echo, "- tools.list_mcp_resources: ", "- tools.read_mcp_resource: "] {
        assert!(reenabled.contains(entry), "re-enabling omitted {entry}");
    }
    assert_eq!(
        amendments[10],
        [format!("<nested_tools>\nAdded or changed:\n{revised}\n</nested_tools>")]
    );
    // Namespace guidance, eager/deferred movement, an unchanged catalogue, frozen exec
    // sections and compaction leave the roster alone.
    for index in [2, 4, 5, 6, 7, 11, 12] {
        assert!(amendments[index].is_empty(), "request {index}");
    }
    // The new window states the current roster once.
    let [roster] = amendments[13].as_slice() else {
        panic!("a window opens with one roster: {:?}", amendments[13])
    };
    for entry in [cwd, echo, revised] {
        assert!(roster.contains(entry), "the new window omitted {entry}");
    }
    // Native declarations are fingerprinted in snapshots; keep their invariants readable here.
    let mut native_fields = declarations[0][0].clone();
    native_fields["description"] = json!(null);
    for exec in declarations.iter().flatten() {
        let mut actual = exec.clone();
        actual["description"] = json!(null);
        assert_eq!(actual, native_fields, "compaction preserves native fields");
    }
    for request in &requests[10..13] {
        let retained_input = serde_json::to_string(&request.input())?;
        for frozen in [
            "Run JavaScript. Await every tool call",
            "Find deferred tools in ALL_TOOLS.",
            "Find deferred tools by name in ALL_TOOLS.",
            "content: unknown[]; structuredContent?: T; isError?: boolean",
        ] {
            assert!(
                !retained_input.contains(frozen),
                "frozen documentation appeared before compaction: {frozen}"
            );
        }
    }
    let restored = declarations[13][0]["description"]
        .as_str()
        .expect("compacted catalog");
    for expected in [
        "Await every tool call and inspect its result",
        "Find deferred tools by name in ALL_TOOLS.",
        "isError?: boolean; _meta?: unknown",
    ] {
        assert!(restored.contains(expected), "compaction omitted {expected}");
    }
    assert!(
        !restored.contains(updated_instructions),
        "Glossed namespace guidance belongs to on-demand contracts"
    );
    // Full schemas belong to explicit discovery, not the eager CodeModeOnly inventory.
    let contracts = responses::mount_sse_sequence(&server, vec![
        responses::sse(vec![
            responses::ev_custom_tool_call("contracts", "exec", r#"text({resource: describe_tool("read_mcp_resource"), echo: describe_tool("mcp__echo_service__echo"), cwd: describe_tool("mcp__echo_service__cwd")});"#),
            responses::ev_completed("contracts"),
        ]),
        responses::sse_completed("contracts-done"),
    ]).await;
    test.submit_text_turn("Inspect the current resource and MCP call contracts.")
        .await?;
    let contract_text = super::super::code_mode::custom_tool_output_last_non_empty_text(
        &contracts.last_request().expect("contract result"),
        "contracts",
    )
    .expect("on-demand contracts");
    let contracts: serde_json::Value = serde_json::from_str(&contract_text)?;
    assert!(
        contracts["resource"]
            .as_str()
            .expect("resource contract")
            .contains("revision (optional): integer")
    );
    let echo_contract = contracts["echo"].as_str().expect("echo contract");
    assert!(echo_contract.contains("message (required): string"));
    assert!(echo_contract.contains(updated_instructions));
    assert!(
        contracts["cwd"]
            .as_str()
            .expect("cwd contract")
            .contains("Parameters: object")
    );
    let scenario = "A catalogue change amends the nested-tool roster in one developer message and declares nothing: additions, removals, remove-all and re-enabling preserve context, while namespace guidance and eager/deferred movement leave the roster alone. Common instructions and shared types stay frozen; compaction keeps the retained prefix, and the new window installs the latest declaration and the current roster.";
    let snapshot = context_snapshot::format_request_history_snapshot(
        scenario,
        &requests,
        &ContextSnapshotOptions::default().rewrite_known_segments(),
    );
    insta::assert_snapshot!("incremental_exec_tools", snapshot);
    Ok(())
}
