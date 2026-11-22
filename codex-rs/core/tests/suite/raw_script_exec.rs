use super::create_workspace_directory;
use super::parse_unified_exec_output;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::protocol::TurnEnvironmentSelections;
use codex_protocol::user_input::UserInput;
use codex_utils_path_uri::PathUri;
use core_test_support::exec_server::ExecServerProcess;
use core_test_support::mock_server;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::test_codex::environment_config_for_selection;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test_case::test_case("python", false; "python_foreground")]
#[test_case::test_case("bash", false; "bash_foreground")]
#[test_case::test_case("python", true; "python_background")]
#[test_case::test_case("bash", true; "bash_background")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_script_execution_contract(language: &str, background: bool) -> Result<()> {
    core_test_support::skip_if_target_windows!(Ok(()), "POSIX Bash and python3 fixtures");
    core_test_support::skip_if_no_network!(Ok(()));
    core_test_support::skip_if_sandbox!(Ok(()));

    let server = responses::start_mock_server().await;
    Box::pin(mock_server::run(&[&server], async {
    let mut builder = test_codex()
        .with_code_mode_host_program("codex-code-mode-host-does-not-exist".into())
        .with_config(|config| {
            for feature in [Feature::CodeModeOnly, Feature::CodexHooks] {
                config.features.enable(feature).expect("enable script hooks");
            }
            config
                .permissions
                .set_permission_profile(PermissionProfile::read_only())
                .expect("owner environments inherit the reviewed read-only sandbox");
        });
    let test = builder.build_with_auto_env(&server).await?;
    let workdir = create_workspace_directory(&test, "script-cwd").await?;
    let mut executor = None;
    let executor_url = if let Some(url) = test.executor_environment().exec_server_url() {
        url
    } else {
        &executor.insert(ExecServerProcess::start().await?).websocket_url
    };
    let primary = test.executor_environment().selection().clone();
    let mut secondary = primary.clone();
    secondary.environment_id = "script-owner".to_string();
    test.thread_manager.environment_manager().upsert_environment(
        secondary.environment_id.clone(),
        executor_url.to_owned(),
        /*connect_timeout*/ None,
    )?;
    let mut selections = vec![primary, secondary];
    for (index, selection) in selections.iter_mut().enumerate() {
        let command = if index == 0 {
            r#"printf '%s' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"unrelated executor hook"}}'"#
        } else {
            // Echo the hook's actual input: hook processing must preserve the raw metadata's executor.
            r#"python3 -c 'import json, sys; incoming = json.load(sys.stdin); print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": incoming["tool_input"], "additionalContext": "raw script owner hook"}}))'"#
        };
        let mut config = environment_config_for_selection(&test.config, selection);
        config.environment_hooks = serde_json::from_value(json!({
            "PreToolUse": [{
                "selector": {"type": "tool", "namespace": "^functions$", "name": format!("^{language}$")},
                "hooks": [{"type": "command", "command": command}],
            }],
        }))?;
        selection.config = EnvironmentConfigState::Ready(config);
    }
    let metadata = json!({
        "environment_id": selections[1].environment_id,
        "workdir": "script-cwd", "max_output_tokens": 500, "background": background,
        "sandbox_permissions": "require_escalated", "justification": "Review the complete raw script",
    });
    let (interpreter, flags, body) = match language {
        "python" => (
            "python3",
            vec!["-u", "-c"],
            "import os\nprint(\"quote=' double=\\\" slash=\\\\ Unicode=λ\")\nprint(os.path.basename(os.getcwd()))\n",
        ),
        "bash" => (
            "bash",
            vec!["-c"],
            "printf '%s\\n' \"quote=' double=\\\" slash=\\\\ Unicode=λ\"\nprintf '%s\\n' \"${PWD##*/}\"\n",
        ),
        _ => unreachable!("known language fixture"),
    };
    let delay = if background {
        if language == "python" {
            "import pathlib, time\nfor _ in range(200):\n    if pathlib.Path('release').exists():\n        break\n    time.sleep(0.05)\nelse:\n    raise TimeoutError('release gate timed out')\n"
        } else {
            "for i in {1..200}; do\n    [[ -e release ]] && break\n    sleep 0.05\ndone\n[[ -e release ]] || exit 124\n"
        }
    } else {
        ""
    };
    let source = format!("#!/usr/bin/env {interpreter}\n# @exec: {metadata}\n{delay}{body}");
    let expected_command = std::iter::once(interpreter.to_string())
        .chain(flags.iter().map(|flag| (*flag).to_string()))
        .chain(std::iter::once(source.clone()))
        .collect::<Vec<_>>();
    // Metadata failures must never reach process execution or approval, even with executable source following them.
    let invalid = [
        "# @exec: {",
        "# @exec: {\"background\":true,\"background\":false}",
        "# @exec: {\"timeout_ms\":1}",
        "# @exec: {\"background\":\"true\"}",
        "# @exec: {\"workdir\":null}",
        "# @exec: {\"sandbox_permissions\":\"with_additional_permissions\"}",
        "# @exec: {\"max_output_tokens\":-1}",
    ];
    let mut first = vec![ev_response_created("resp-1")];
    for (index, directive) in invalid.iter().enumerate() {
        first.push(ev_custom_tool_call(
            &format!("invalid-{index}"),
            language,
            &format!("{directive}\n{body}"),
        ));
    }
    first.push(ev_custom_tool_call("script", language, &source));
    first.push(ev_completed("resp-1"));
    let log = responses::mount_sse_sequence(
        &server,
        vec![
            sse(first),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Run the raw source directly".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                environments: Some(TurnEnvironmentSelections::new(test.config.cwd.clone(), selections).into_requests()),
                approval_policy: Some(AskForApproval::OnRequest),
                sandbox_policy: Some(SandboxPolicy::ReadOnly {
                    network_access: false,
                }),
                ..Default::default()
            }),
        )
        .await?;
    let mut approvals = 0;
    let mut launches = 0;
    let mut captured = std::collections::HashMap::new();
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::ItemCompleted(event) => {
                if let codex_protocol::items::TurnItem::ToolInput(input) = event.item {
                    captured.insert(input.call_id.clone(), input);
                }
            }
            EventMsg::ExecApprovalRequest(approval) => {
                assert_eq!(approval.call_id, "script");
                assert_eq!(approval.command, expected_command);
                assert!(approval.proposed_execpolicy_amendment.is_none());
                approvals += 1;
                test.codex
                    .submit(Op::ExecApproval {
                        id: approval.effective_approval_id(),
                        turn_id: Some(approval.turn_id),
                        decision: ReviewDecision::Approved,
                    })
                    .await?;
            }
            EventMsg::ExecCommandBegin(begin) => {
                assert_eq!(begin.call_id, "script");
                assert_eq!(begin.command, expected_command);
                assert_eq!(begin.cwd, PathUri::from_host_native_path(&workdir)?);
                launches += 1;
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    assert_eq!((approvals, launches), (1, 1));
    assert_eq!(captured["script"].payload.text(), source);
    assert_eq!(captured["script"].name.name, language);
    assert_eq!(captured.len(), invalid.len() + 1);
    for (index, directive) in invalid.iter().enumerate() {
        assert_eq!(captured[&format!("invalid-{index}")].payload.text(), format!("{directive}\n{body}"));
    }
    let requests = log.requests();
    let tools = requests[0].body_json()["tools"]
        .as_array()
        .expect("model tools")
        .clone();
    for name in ["exec", "python", "bash", "write_stdin"] {
        assert!(
            tools.iter().any(|tool| tool["name"] == name),
            "direct visibility: {name}"
        );
    }
    let request = requests.last().expect("follow-up request");
    assert!(request.message_input_texts("developer").iter().any(|text| text.contains("raw script owner hook")),
        "only the metadata-selected executor hook should accept the immutable script");
    for index in 0..invalid.len() {
        let output = request.custom_tool_call_output(&format!("invalid-{index}"));
        assert!(
            output["output"]
                .as_str()
                .expect("rejection text")
                .contains("Invalid # @exec metadata")
        );
    }
    let result = request.custom_tool_call_output("script");
    let mut output = parse_unified_exec_output(result["output"].as_str().expect("native result"))?;
    if background {
        assert!(
            output.process_id.is_some(),
            "expected live script: {result}"
        );
        test.fs()
            .write_file(
                &PathUri::from_host_native_path(workdir.join("release"))?,
                Vec::new(),
                codex_exec_server::WriteFileOptions::default(),
                /*sandbox*/ None,
            )
            .await?;
        let continuation = responses::mount_sse_sequence(&server, vec![
            sse(vec![ev_response_created("resp-3"), ev_function_call("stdin", "write_stdin", &json!({ "session_id": output.process_id.as_ref().expect("live process").parse::<i32>()?, "chars": "", "yield_time_ms": 5000 }).to_string()), ev_completed("resp-3")]),
            sse(vec![ev_response_created("resp-4"), ev_assistant_message("msg-2", "done"), ev_completed("resp-4")]),
        ]).await;
        test.submit_turn("Continue the raw script through write_stdin")
            .await?;
        let requests = continuation.requests();
        let result = requests
            .last()
            .expect("continuation result")
            .function_call_output("stdin");
        output =
            parse_unified_exec_output(result["output"].as_str().expect("native stdin result"))?;
    }
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.process_id, None);
    assert_eq!(
        output.output,
        "quote=' double=\" slash=\\ Unicode=λ\nscript-cwd\n"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
    }))
    .await
}
