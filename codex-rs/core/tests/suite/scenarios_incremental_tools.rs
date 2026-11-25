//! Request-history coverage for incremental tools on Responses and Responses Lite.

use anyhow::Result;
use codex_core::ForkSnapshot;
use codex_core::McpServersDisabled;
use codex_core::StartThreadOptions;
use codex_core::compact::AutoCompactionDisabled;
use codex_features::Feature;
use codex_history::InitialHistory;
use codex_history::ResumedHistory;
use codex_history::RolloutItem;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_protocol::openai_models::CodeModeToolMessages;
use codex_protocol::openai_models::IncrementalToolMessages;
use codex_protocol::openai_models::ToolMessage;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use core_test_support::context_snapshot;
use core_test_support::context_snapshot::ContextSnapshotOptions;
use core_test_support::mock_server;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use test_case::test_case;

use super::super::rmcp_client::remote_aware_environment_id;
use super::super::rmcp_client::remote_aware_stdio_server_bin;

#[test_case(false; "update MCP declarations when disabled")]
#[test_case(true; "preserve inherited declarations by default")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prediction_fork_keeps_inherited_tools(preserve_parent_tools: bool) -> Result<()> {
    skip_if_wine_exec!(
        Ok(()),
        "requires a Windows test_stdio_server in the Wine-exec environment"
    );
    skip_if_no_network!(Ok(()));
    let server = core_test_support::responses::start_mock_server().await;
    core_test_support::mock_server::run(&[&server], async {
        let mcp = serde_json::from_value(json!({
            "command": remote_aware_stdio_server_bin()?,
            "environment_id": remote_aware_environment_id(),
            "enabled_tools": ["echo"],
            "required": true,
        }))?;
        let mut test = test_codex()
            .with_model_info_override("gpt-5.4", |model| {
                model.use_responses_lite = true;
                model.supports_search_tool = false;
                model.tool_mode = Some(ToolMode::CodeModeOnly);
            })
            .with_config(move |config| {
                config.workspace_roots = vec![config.cwd.clone()];
                config.base_instructions = Some("Use the available tools to help the user.".into());
                config.features.enable(Feature::IncrementalTools)
                    .expect("enable incremental tools");
                if !preserve_parent_tools {
                    config.features.disable(Feature::PredictionPreserveParentTools)
                        .expect("disable inherited prediction tools");
                }
                config.mcp_servers.set(std::collections::HashMap::from([(
                    "echo_service".into(),
                    mcp,
                )])).expect("set MCP server");
            })
            .build_with_auto_env(&server)
            .await?;
        wait_for_mcp_server(&test.codex, "echo_service").await?;
        let mock = responses::mount_sse_sequence(
            &server,
            ["parent", "child", "child-again", "parent-again"]
                .into_iter()
                .map(|id| responses::sse(vec![responses::ev_completed(id)]))
                .collect(),
        ).await;
        test.submit_text_turn("Start with the echo service.").await?;

        let parent = std::sync::Arc::clone(&test.codex);
        parent.flush_rollout().await?;
        let (mut options, settings) = test.thread_manager
            .fork_options_from_parent(test.session_configured.thread_id).await?;
        // Match experimentalPredictionMode with experimentalDisablePredictionMcp enabled.
        options.config.ephemeral = true;
        options.config.features.enable(Feature::ReasoningEffortOverride)?;
        options.thread_extension_init.insert(AutoCompactionDisabled);
        options.thread_extension_init.insert(McpServersDisabled);
        let child = test.thread_manager.fork_legacy_thread(
            ForkSnapshot::Interrupted,
            options,
            parent.rollout_path().expect("parent rollout"),
        ).await?;
        child.thread.restore_thread_settings(settings).await?;
        test.codex = child.thread;
        test.submit_text_turn("Predict the next message.").await?;
        test.submit_text_turn("Predict again.").await?;
        test.codex = parent;
        test.submit_text_turn("Continue using the echo service.").await?;

        let requests = mock.requests();
        assert_eq!(requests.len(), 4);
        let parent_input = requests[0].input();
        let child_input = requests[1].input();
        assert!(child_input.starts_with(&parent_input));
        assert!(requests[2].input().starts_with(&child_input));

        let inherited = requests[0].inputs_of_type("additional_tools");
        let roster = |index: usize| {
            core_test_support::nested_tools::fragments(&requests[index].input())
        };
        assert!(roster(0).concat().contains("mcp__echo_service__echo"));

        // The roster is an instruction, so no fork redeclares `exec` to change it.
        for request in &requests[1..] {
            assert_eq!(request.inputs_of_type("additional_tools"), inherited);
        }
        if preserve_parent_tools {
            assert_eq!(roster(1), roster(0), "the inherited roster stands under the notice");
            assert!(!child_input.iter().any(|item|
                item.to_string().contains("no longer available")
            ));
        } else {
            let child = roster(1);
            let [inherited_roster, amendment] = child.as_slice() else {
                panic!("the fork amends the roster once: {child:?}")
            };
            assert_eq!(std::slice::from_ref(inherited_roster), roster(0).as_slice());
            assert!(amendment.starts_with("<nested_tools>\nRemoved:\n"), "{amendment}");
            assert!(amendment.contains("mcp__echo_service__echo"), "{amendment}");
        }
        assert_eq!(roster(2), roster(1), "the second turn repeats nothing");
        assert_eq!(roster(3), roster(0), "the parent keeps its roster");

        let unavailable_notices: Vec<_> = requests.iter().map(|request| {
            request.message_input_texts("developer").iter().filter(|text|
                text.contains("MCP tools are unavailable for this request.")
            ).count()
        }).collect();
        let notice_count = usize::from(preserve_parent_tools);
        assert_eq!(unavailable_notices, [0, notice_count, notice_count, 0]);

        let (snapshot_name, scenario) = if preserve_parent_tools {
            (
                "prediction_fork_inherited_tools",
                "The prediction fork preserves the parent's complete prefix and tool declarations, then appends one MCP-unavailable notice. Its second turn retains that notice without another update; continuing the parent keeps its original tools.",
            )
        } else {
            (
                "prediction_fork_mcp_removal",
                "The prediction fork preserves the parent's complete prefix, then amends the nested-tool roster to drop the unavailable MCP helpers. Its second turn retains that amendment without repeating it; continuing the parent keeps its original tools.",
            )
        };
        insta::assert_snapshot!(
            snapshot_name,
            context_snapshot::format_request_history_snapshot(
                scenario,
                &requests,
                &ContextSnapshotOptions::default()
                    .rewrite_known_segments()
                    .include_request_settings(),
            )
        );
        Ok(())
    }).await
}

#[test_case(true, None, true, OPENAI_PROVIDER_ID, &[]; "responses_lite")]
#[test_case(true, Some(true), true, OPENAI_PROVIDER_ID, &[]; "announcements_suppressed")]
#[test_case(false, None, false, OPENAI_PROVIDER_ID, &[]; "responses_api")]
#[test_case(false, None, false, OPENAI_PROVIDER_ID,
    &[(Feature::IncrementalToolsLegacy, true)]; "responses_ignores_unsupported_legacy_flag")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_tools_append_changed_catalog_without_rewriting_history(
    use_responses_lite: bool,
    suppress_model_change_announcements: Option<bool>,
    incremental_tools: bool,
    provider_id: &'static str,
    feature_overrides: &'static [(Feature, bool)],
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    mock_server::run(&[&server], async {
    let mock = responses::mount_sse_sequence(
        &server,
        (1..=if incremental_tools { 7 } else { 4 })
            .map(|index| {
                let mut events = Vec::new();
                if index == 5 {
                    events.push(json!({
                        "type": "response.output_item.done",
                        "item": {
                            "type": "compaction",
                            "encrypted_content": "INCREMENTAL_TOOLS_CHECKPOINT",
                        },
                    }));
                }
                events.push(responses::ev_completed(&format!("resp-{index}")));
                responses::sse(events)
            })
            .collect(),
    )
    .await;
    let test = test_codex()
        .with_model_info_override("gpt-5.4", move |model| {
            model.use_responses_lite = use_responses_lite;
            model.tool_mode = Some(ToolMode::CodeMode);
            if incremental_tools {
                model
                    .model_messages
                    .as_mut()
                    .expect("model messages")
                    .instructions_template =
                    Some("Use the available tools to help the user.".to_string());
            }
        })
        .with_config(move |config| {
            config.model_provider_id = provider_id.to_string();
            for &(feature, enabled) in feature_overrides {
                config
                    .features
                    .set_enabled(feature, enabled)
                    .expect("configure independent provider flags");
            }
            if let Some(suppress) = suppress_model_change_announcements {
                config
                    .features
                    .set_enabled(Feature::SuppressModelChangeAnnouncements, suppress)
                    .expect("configure model announcements");
            }
            if !incremental_tools {
                config.base_instructions =
                    Some("Use the available tools to help the user.".to_string());
            }
            config.code_mode.disable_in_process_fallback = true;
            let catalog = config.model_catalog.as_mut().expect("model catalog");
            let mut updated = catalog
                .models
                .iter()
                .find(|model| model.slug == "gpt-5.4")
                .expect("source model")
                .clone();
            updated.slug = "updated-tools-model".to_string();
            if incremental_tools {
                updated
                    .model_messages
                    .as_mut()
                    .expect("model messages")
                    .instructions_template = Some("Review results before responding.".to_string());
            }
            updated
                .model_messages
                .get_or_insert_default()
                .tools
                .get_or_insert_default()
                .code_mode = Some(CodeModeToolMessages {
                exec: Some(ToolMessage {
                    description: Some("Updated execution instructions.".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            });
            catalog.models.push(updated);
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("Begin work.").await?;
    test.submit_turn("Continue with the same tools.").await?;
    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model: Some("updated-tools-model".to_string()),
            ..Default::default()
        },
    )
    .await?;
    test.submit_text_turn("Continue with the updated execution instructions.")
        .await?;
    test.submit_text_turn("Continue with the same updated tools.")
        .await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    if !incremental_tools {
        for request in &requests {
            let body = request.body_json();
            assert!(
                body["tools"]
                    .as_array()
                    .is_some_and(|tools| !tools.is_empty())
            );
            assert_eq!(
                request.instructions_text(),
                "Use the available tools to help the user."
            );
        }
        assert_eq!(
            requests[0].body_json()["tools"],
            requests[1].body_json()["tools"]
        );
        assert_ne!(
            requests[1].body_json()["tools"],
            requests[2].body_json()["tools"]
        );
        assert_eq!(
            requests[2].body_json()["tools"],
            requests[3].body_json()["tools"]
        );
        return Ok(());
    }
    assert!(requests[1].input().starts_with(&requests[0].input()));
    assert!(requests[2].input().starts_with(&requests[1].input()));
    assert!(requests[3].input().starts_with(&requests[2].input()));
    for request in &requests {
        if let Some(tools) = request.body_json()["tools"].as_array() {
            assert!(tools.iter().all(|tool| tool["type"] == "web_search"));
        }
        assert!(request.inputs_of_type("additional_tools").iter()
            .flat_map(|item| item["tools"].as_array().expect("declarations"))
            .all(|tool| tool["type"] != "web_search"));
    }
    for request in &requests {
        let input = request.input();
        assert_eq!(input[0]["type"], "additional_tools");
        assert_eq!(input[1]["role"], "developer");
        assert_eq!(
            input[1]["content"][0]["text"],
            "Use the available tools to help the user."
        );
        assert!(request.body_json().get("instructions").is_none());
    }
    let initial = requests[0].inputs_of_type("additional_tools");
    assert_eq!(initial.len(), 1);
    assert_eq!(
        initial[0]["tools"]
            .as_array()
            .expect("initial tool declarations")
            .iter()
            .map(|tool| tool["type"].clone())
            .collect::<Vec<_>>(),
        vec![json!("namespace"), json!("tool_search")]
    );
    assert_eq!(requests[1].inputs_of_type("additional_tools"), initial);
    let changed = requests[2].inputs_of_type("additional_tools");
    assert_eq!(requests[2].body_json()["model"], "updated-tools-model");
    assert_eq!(&changed[..initial.len()], &initial);
    assert_eq!(changed.len(), initial.len() + 1);
    let diff = changed.last().expect("changed tool declarations")["tools"]
        .as_array()
        .expect("changed tool array");
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0]["name"], "functions");
    assert_eq!(initial[0]["tools"][0]["description"], json!(""));
    assert_eq!(
        diff[0]["description"],
        json!(
            ""
        )
    );
    assert!(requests[2].body_contains_text("This is an incremental tools update."));
let changed_tools = diff[0]["tools"].as_array().expect("namespace members");
    assert_eq!(changed_tools.len(), 1);
    assert_eq!(changed_tools[0]["name"], "exec");
    assert!(
        changed
            .last()
            .expect("changed tool declarations")
            .to_string()
            .contains("Updated execution instructions.")
    );
    assert_eq!(requests[3].inputs_of_type("additional_tools"), changed);

    test.codex.ensure_rollout_materialized().await;
    test.codex.flush_rollout().await?;
    let rollout =
        codex_rollout::read_rollout_text(test.codex.rollout_path().expect("rollout path"))?;
    let rollout = rollout
        .lines()
        .map(codex_rollout::parse_rollout_line)
        .collect::<Result<Vec<_>, _>>()?;
    // History-carried tools still belong to the human prefix census; later catalog
    // deltas are arrivals, not a replacement prefix or an invisible tool budget.
    let preambles = rollout
        .iter()
        .filter_map(|line| match &line.item {
            RolloutItem::EventMsg(codex_protocol::protocol::EventMsg::ItemCompleted(event)) => {
                match &event.item {
                    codex_protocol::items::TurnItem::Preamble(preamble) => Some(preamble),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(preambles.len(), 1);
    assert!(
        preambles[0]
            .parts
            .iter()
            .any(|part| part.section == codex_protocol::PreambleSection::Tools && matches!(part.tokens, codex_protocol::CensusCount::Counted(tokens) if tokens > 0))
    );
    assert!(preambles[0].parts.iter().any(|part| part.section
        == codex_protocol::PreambleSection::Instructions
        && matches!(part.tokens, codex_protocol::CensusCount::Counted(tokens) if tokens > 0)));
    let catalogs = rollout
        .iter()
        .filter_map(|line| match &line.item {
            RolloutItem::WorldState(item) => item.state.get("top_level_tools").cloned(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(catalogs.len(), 2);
    assert_ne!(catalogs[0], catalogs[1]);
    assert_eq!(
        catalogs[1]
            .as_object()
            .expect("updated tool catalog")
            .iter()
            .filter(|(key, hash)| catalogs[0].get(*key) != Some(*hash))
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        vec!["functions.exec"]
    );
    assert!(catalogs.iter().all(|catalog| {
        catalog
            .as_object()
            .expect("tool catalog")
            .values()
            .all(|hash| hash.as_str().is_some_and(|hash| hash.len() == 40))
    }));
    // Upstream's new compaction interaction must retain the fork's canonical
    // instruction projection and rebuild the current catalog in the next window.
    let requests = if incremental_tools {
        test.codex.submit(Op::Compact).await?;
        wait_for_event(&test.codex, |event| {
            if let EventMsg::TurnComplete(completed) = event {
                assert!(completed.error.is_none(), "{completed:?}");
                true
            } else {
                false
            }
        }).await;
        test.submit_text_turn("Continue after compaction.").await?;
        let requests = mock.requests();
        assert_eq!(requests.len(), 6);
        assert!(requests[4].body_contains_text("Review results before responding."));
        assert!(!requests[4].body_contains_text("<instruction_diff>"));
        assert!(requests[5].body_contains_text("INCREMENTAL_TOOLS_CHECKPOINT"));
        assert!(!requests[5].body_contains_text("Use the available tools to help the user."));
        assert_eq!(requests[5].instructions_text(), "Review results before responding.");
        assert_eq!(requests[5].inputs_of_type("additional_tools").len(), 1);
        requests
    } else {
        requests
    };
    insta::assert_snapshot!(
        if suppress_model_change_announcements == Some(true) {
            "incremental_tools_suppressed_announcements"
        } else if incremental_tools && use_responses_lite {
            "incremental_tools"
        } else if incremental_tools {
            "incremental_tools_regular_responses"
        } else {
            "incremental_tools_responses_api"
        },
        context_snapshot::format_request_history_snapshot(
            if suppress_model_change_announcements == Some(true) {
                "Model-ID notices are suppressed while changed tools and a base-instructions amendment append without rewriting history."
            } else if incremental_tools {
                "Tool definitions precede the initial base instructions. A model switch appends a tools notice, changed tools, a base-instructions amendment, and an identity-only model notice without rewriting the prefix; the next unchanged turn appends only user input."
            } else {
                "Regular Responses requests carry the full current tool catalog without an incremental notice; changed execution instructions replace the catalog on subsequent requests."
            },
            &requests,
            &ContextSnapshotOptions::default()
                .rewrite_known_segments()
                .include_request_settings(),
        )
    );
    // Ephemeral side forks still have a resident frontend. Persistence alone must not
    // remove live tools and re-inject exec's otherwise unchanged embedded inventory.
    let mut fork_config = test.config.clone();
    fork_config.model = Some("updated-tools-model".to_string());
    fork_config.ephemeral = true;
    fork_config.daybreak_enabled = false;
    fork_config.developer_instructions = Some("Answer only the side question.".to_string());
    let mut fork_history = rollout.iter().map(|line| line.item.clone()).collect::<Vec<_>>();
    // Snapshot the completed sampling step before its turn terminator, as /btw
    // does when the parent's response is still in progress. Never alter the source.
    let terminator = fork_history.iter().rposition(|item|
        matches!(item, RolloutItem::EventMsg(EventMsg::TurnComplete(_)))
    ).expect("parent turn terminator");
    fork_history.remove(terminator);
    let forked = test.thread_manager.fork_thread_from_history(
        ForkSnapshot::Interrupted,
        StartThreadOptions::new(fork_config),
        InitialHistory::Resumed(ResumedHistory {
            history_revision: None,
            conversation_id: test.session_configured.thread_id,
            history: Arc::new(fork_history),
            rollout_path: None,
        }),
    ).await?;
    forked.thread.start_or_steer_turn(
        TurnInputRequest::user_input(vec![
            UserInput::Text {
                text: "Side question.".to_string(),
                text_elements: Vec::new(),
            },
        ]),
    ).await?;
    let mut fork_items = Vec::new();
    wait_for_event(&forked.thread, |event| {
        if let EventMsg::ItemCompleted(event) = event {
            fork_items.push(event.item.clone());
        }
        matches!(event, EventMsg::TurnComplete(_))
    }).await;
    let fork_requests = mock.requests();
    assert_eq!(fork_requests.len(), requests.len() + 1);
    assert_eq!(fork_requests[requests.len()].inputs_of_type("additional_tools"), changed);
    let fork_input = fork_requests[requests.len()].input();
    assert!(fork_input.starts_with(&requests[3].input()));
    let boundary = fork_input.iter().position(|item|
        item.to_string().contains("<turn_aborted>")
    ).expect("interrupted fork boundary");
    let question = fork_input.iter().position(|item|
        item["content"][0]["text"] == "Side question."
    ).expect("side question");
    assert!(boundary < question);
    let user_source = fork_items.iter().find_map(|item| match item {
        codex_protocol::items::TurnItem::UserMessage(user) => user.source_response_item_id.as_ref(),
        _ => None,
    }).expect("human receipt links to canonical input");
    assert_eq!(fork_input[question]["id"].as_str(), Some(user_source.as_str()));
    let arrivals = fork_items.iter().filter_map(|item| match item {
        codex_protocol::items::TurnItem::Arrival(arrival) => Some(arrival),
        _ => None,
    }).collect::<Vec<_>>();
    // No measured child step precedes this first request. The interruption stays in
    // model history above, but must not borrow the parent's step for an Arrival.
    assert!(arrivals.is_empty(), "a fresh child cannot emit parent-step arrivals: {arrivals:?}");

    Ok(())
    })
    .await
}

#[test_case(false, true; "bundled")]
#[test_case(true, true; "catalog_messages")]
#[test_case(false, false; "responses_api")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_removal_appends_a_standalone_notice_without_rewriting_history(
    catalog_messages: bool,
    use_responses_lite: bool,
) -> Result<()> {
    skip_if_wine_exec!(
        Ok(()),
        "requires a Windows test_stdio_server in the Wine-exec environment"
    );
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    mock_server::run(&[&server], async {
    let command = remote_aware_stdio_server_bin()?;
    let environment_id = remote_aware_environment_id();
    let server_config = |enabled_tools: &[&str], instructions: &str| {
        serde_json::from_value(json!({
            "command": command,
            "environment_id": environment_id,
            "env": {"MCP_TEST_SERVER_INSTRUCTIONS": instructions},
            "enabled_tools": enabled_tools,
            "default_tools_approval_mode": "approve",
        }))
    };
    let initial_server = server_config(&["echo", "cwd"], "Echo service instructions.")?;
    let reduced_server = server_config(&["echo"], "Echo service instructions.")?;
    let restored_server = server_config(&["echo", "cwd"], "Echo service instructions.")?;
    let test = test_codex()
        .with_model_info_override("gpt-5.5", move |model| {
            model.use_responses_lite = use_responses_lite;
            model.supports_search_tool = false;
            model.tool_mode = Some(ToolMode::Direct);
            if catalog_messages {
                model
                    .model_messages
                    .get_or_insert_default()
                    .tools
                    .get_or_insert_default()
                    .incremental_tools = Some(IncrementalToolMessages {
                    tool_update_hint: Some(
                        "Catalog update: retain earlier tools unless explicitly removed."
                            .to_string(),
                    ),
                    removed_tools_header: Some("Unavailable tools:".to_string()),
                    removed_namespaces_header: Some("Unavailable namespaces:".to_string()),
                    namespace_instructions_prefix: Some("Catalog {name}: ".to_string()),
                    namespace_instructions_cleared: Some("Cleared {name}.".to_string()),
                    ..Default::default()
                });
            }
        })
        .with_config(move |config| {
            if !use_responses_lite {
                config.model_provider_id = OPENAI_PROVIDER_ID.to_string();
            }
            config
                .features
                .enable(if use_responses_lite {
                    Feature::IncrementalTools
                } else {
                    Feature::IncrementalToolsLegacy
                })
                .expect("enable incremental tools");
            if catalog_messages {
                config
                    .features
                    .enable(Feature::NonPrefixedMcpToolNames)
                    .expect("enable unprefixed MCP namespaces");
                config.non_prefixed_mcp_tool_servers = Some(vec!["functions".to_string()]);
            }
            config
                .mcp_servers
                .set(std::collections::HashMap::from([(
                    "echo_service".to_string(),
                    initial_server,
                )]))
                .expect("set MCP fixture");
        })
        .build_with_auto_env(&server)
        .await?;
    wait_for_mcp_server(&test.codex, "echo_service").await?;
    let response_count = if catalog_messages { 11 } else { 7 };
    let mock = responses::mount_sse_sequence(
        &server,
        (1..=response_count)
            .map(|index| responses::sse(vec![responses::ev_completed(&format!("resp-{index}"))]))
            .collect(),
    )
    .await;
    test.submit_text_turn("Begin with the echo service.")
        .await?;

    // Remove one member while keeping the namespace instructions and echo schema unchanged.
    let current_config = test.codex.config().await;
    let mut reduced_config = current_config.as_ref().clone();
    reduced_config
        .mcp_servers
        .set(std::collections::HashMap::from([(
            "echo_service".to_string(),
            reduced_server,
        )]))?;
    let _ = test
        .codex
        .refresh_mcp_config(current_config, reduced_config)
        .await;
    // Reconcile the refreshed runtime through a public call before the next model request.
    test.codex
        .call_mcp_tool(
            "echo_service",
            /*environment_id*/ None,
            "echo",
            Some(json!({"message": "ready after removal"})),
            /*meta*/ None,
        )
        .await?;
    test.submit_text_turn("Continue after disabling the cwd tool.")
        .await?;
    test.submit_text_turn("Continue with the remaining echo tool.")
        .await?;

    // Remove the whole server, then reconcile even though its tools can no longer be called.
    let current_config = test.codex.config().await;
    let mut removed_config = current_config.as_ref().clone();
    removed_config.mcp_servers.set(Default::default())?;
    let _ = test
        .codex
        .refresh_mcp_config(current_config, removed_config)
        .await;
    let _ = test
        .codex
        .call_mcp_tool(
            "echo_service",
            /*environment_id*/ None,
            "echo",
            Some(json!({"message": "unavailable"})),
            /*meta*/ None,
        )
        .await;
    test.submit_text_turn("Continue after removing the echo service.")
        .await?;
    test.submit_text_turn("Continue without the echo service.")
        .await?;

    // Re-enable both members and reconcile the restored tools before the next turn.
    let current_config = test.codex.config().await;
    let mut restored_config = current_config.as_ref().clone();
    restored_config
        .mcp_servers
        .set(std::collections::HashMap::from([(
            "echo_service".to_string(),
            restored_server,
        )]))?;
    let _ = test
        .codex
        .refresh_mcp_config(current_config, restored_config)
        .await;
    for (tool, arguments) in [
        ("echo", json!({"message": "ready after restoration"})),
        ("cwd", json!({})),
    ] {
        test.codex
            .call_mcp_tool(
                "echo_service",
                /*environment_id*/ None,
                tool,
                Some(arguments),
                /*meta*/ None,
            )
            .await?;
    }
    test.submit_text_turn("Continue with the restored echo service.")
        .await?;
    test.submit_text_turn("Continue with the same restored tools.")
        .await?;

    if catalog_messages {
        // The functions namespace preserves empty instructions instead of adding fallback text.
        // Move the service there, then change and clear instructions with unchanged tool schemas.
        for (instructions, prompt) in [
            (
                "Use these echo tools.",
                "Continue with the echo tools in the functions namespace.",
            ),
            (
                "Keep {name} and {instructions} literal.",
                "Continue with updated echo instructions.",
            ),
            ("", "Continue after clearing echo instructions."),
        ] {
            let current_config = test.codex.config().await;
            let mut updated_config = current_config.as_ref().clone();
            updated_config
                .mcp_servers
                .set(std::collections::HashMap::from([(
                    "functions".to_string(),
                    server_config(&["echo", "cwd"], instructions)?,
                )]))?;
            let _ = test
                .codex
                .refresh_mcp_config(current_config, updated_config)
                .await;
            test.codex
                .call_mcp_tool(
                    "functions",
                    /*environment_id*/ None,
                    "echo",
                    Some(json!({"message": "ready after instructions refresh"})),
                    /*meta*/ None,
                )
                .await?;
            test.submit_text_turn(prompt).await?;
        }
        test.submit_text_turn("Continue with the same cleared instructions.")
            .await?;
    }

    let requests = mock.requests();
    let (snapshot_name, scenario) = if catalog_messages {
        (
            "incremental_tools_catalog_messages",
            "Catalog overrides supply the update hint, both removal headers, and namespace instructions notices. MCP refreshes remove and restore tools, then change and clear only the namespace instructions. Runtime instructions remain verbatim, and unchanged turns do not repeat notices.",
        )
    } else {
        (
            "incremental_tool_removals",
            "MCP refreshes remove one tool, then its whole namespace, before restoring both tools. Removal notices append alone; restoration appends one developer notice before both namespace declarations. Unchanged turns do not repeat updates.",
        )
    };
    let scenario = if use_responses_lite {
        scenario
    } else {
        "Regular Responses retains its full-current-catalog path across MCP removal and restoration. Genuine catalog changes may rebuild the request prefix; unchanged requests retain the same declarations, and unsupported incremental removal notices are never emitted."
    };
    let snapshot_name = if use_responses_lite {
        snapshot_name
    } else {
        "incremental_tool_removals_regular_responses"
    };
    insta::assert_snapshot!(
        snapshot_name,
        context_snapshot::format_request_history_snapshot(
            scenario,
            &requests,
            &ContextSnapshotOptions::default()
                .rewrite_known_segments()
                .include_request_settings(),
        )
    );
    Ok(())
    })
    .await
}
