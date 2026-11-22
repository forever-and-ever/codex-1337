#![allow(clippy::unwrap_used)]

use anyhow::Context;
use anyhow::Result;
use codex_config::types::ApprovalsReviewer;
use codex_core::CodexThread;
use codex_core::TurnInputRequest;
use codex_core::config::Constrained;
use codex_core::config::ThreadStoreConfig;
use codex_core::sandboxing::SandboxPermissions;
use codex_features::Feature;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecApprovalRequestEvent;
use codex_protocol::protocol::ExecPolicyAmendment;
use codex_protocol::protocol::GranularApprovalConfig;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_apply_patch_custom_tool_call;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::local_requests;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_with_timeout;
use pretty_assertions::assert_eq;
use regex_lite::Regex;
use serde_json::Value;
use serde_json::json;
use std::fs;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use test_case::test_case;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[derive(Clone, Copy)]
enum TargetPath {
    Workspace(&'static str),
    OutsideWorkspace(&'static str),
}

impl TargetPath {
    fn resolve_for_patch(self, test: &TestCodex) -> (PathBuf, String) {
        match self {
            TargetPath::Workspace(name) => {
                let path = test.cwd.path().join(name);
                (path, name.to_string())
            }
            TargetPath::OutsideWorkspace(name) => {
                let path = test.home.path().join(name);
                (path.clone(), path.display().to_string())
            }
        }
    }
}

#[derive(Clone)]
enum ActionKind {
    WriteFile {
        target: TargetPath,
        content: &'static str,
    },
    FetchUrlNoProxy {
        endpoint: &'static str,
        response_body: &'static str,
    },
    FetchUrl {
        endpoint: &'static str,
        response_body: &'static str,
    },
    RunCommand {
        command: &'static str,
    },
    RunCommandWithPolicy {
        command: &'static str,
        policy_src: &'static str,
    },
    RunCommandWithPrefixRule {
        command: &'static str,
        prefix_rule: &'static [&'static str],
    },
    RunUnifiedExecCommand {
        command: &'static str,
        justification: Option<&'static str>,
    },
    ApplyPatchFreeform {
        target: TargetPath,
        content: &'static str,
    },
    ApplyPatchShell {
        target: TargetPath,
        content: &'static str,
    },
}

const DEFAULT_UNIFIED_EXEC_JUSTIFICATION: &str =
    "Requires escalated permissions to bypass the sandbox in tests.";
const WORKSPACE_PERMISSION_PROFILE_CONFIG: &str = r#"default_permissions = ":workspace"
"#;

impl ActionKind {
    fn policy_src(&self) -> Option<&'static str> {
        match self {
            ActionKind::RunCommandWithPolicy { policy_src, .. } => Some(*policy_src),
            ActionKind::WriteFile { .. }
            | ActionKind::FetchUrlNoProxy { .. }
            | ActionKind::FetchUrl { .. }
            | ActionKind::RunCommand { .. }
            | ActionKind::RunCommandWithPrefixRule { .. }
            | ActionKind::RunUnifiedExecCommand { .. }
            | ActionKind::ApplyPatchFreeform { .. }
            | ActionKind::ApplyPatchShell { .. } => None,
        }
    }

    async fn prepare(
        &self,
        test: &TestCodex,
        server: &MockServer,
        call_id: &str,
        sandbox_permissions: SandboxPermissions,
    ) -> Result<(Value, Option<String>)> {
        match self {
            ActionKind::WriteFile { target, content } => {
                let (path, _) = target.resolve_for_patch(test);
                let _ = fs::remove_file(&path);
                let path_str = path.display().to_string();
                let script = format!(
                    "from pathlib import Path; path = Path({path_str:?}); content = {content:?}; path.write_text(content, encoding='utf-8'); print(path.read_text(encoding='utf-8'), end='')",
                );
                let command = format!("python3 -c {script:?}");
                let event = shell_event(
                    call_id,
                    &command,
                    /*timeout_ms*/ 5_000,
                    sandbox_permissions,
                )?;
                Ok((event, Some(command)))
            }
            ActionKind::FetchUrl {
                endpoint,
                response_body,
            } => {
                Mock::given(method("GET"))
                    .and(path(*endpoint))
                    .respond_with(
                        ResponseTemplate::new(200).set_body_string(response_body.to_string()),
                    )
                    .mount(server)
                    .await;

                let url = format!("{}{}", server.uri(), endpoint);
                let escaped_url = url.replace('\'', "\\'");
                let script = format!(
                    "import sys\nimport urllib.request\nurl = '{escaped_url}'\ntry:\n    data = urllib.request.urlopen(url, timeout=2).read().decode()\n    print('OK:' + data.strip())\nexcept Exception as exc:\n    print('ERR:' + exc.__class__.__name__)\n    sys.exit(1)",
                );

                let command = format!("python3 -c \"{script}\"");
                let event = shell_event(
                    call_id,
                    &command,
                    /*timeout_ms*/ 5_000,
                    sandbox_permissions,
                )?;
                Ok((event, Some(command)))
            }
            ActionKind::FetchUrlNoProxy {
                endpoint,
                response_body,
            } => {
                Mock::given(method("GET"))
                    .and(path(*endpoint))
                    .respond_with(
                        ResponseTemplate::new(200).set_body_string(response_body.to_string()),
                    )
                    .mount(server)
                    .await;

                let url = format!("{}{}", server.uri(), endpoint);
                let escaped_url = url.replace('\'', "\\'");
                let script = format!(
                    "import sys\nimport urllib.request\nurl = '{escaped_url}'\nopener = urllib.request.build_opener(urllib.request.ProxyHandler({{}}))\ntry:\n    data = opener.open(url, timeout=2).read().decode()\n    print('OK:' + data.strip())\nexcept Exception as exc:\n    print('ERR:' + exc.__class__.__name__)\n    sys.exit(1)",
                );

                let command = format!("python3 -c \"{script}\"");
                let event = shell_event(
                    call_id,
                    &command,
                    /*timeout_ms*/ 5_000,
                    sandbox_permissions,
                )?;
                Ok((event, Some(command)))
            }
            ActionKind::RunCommand { command } => {
                // Bazel Linux runners can be heavily oversubscribed while this
                // matrix runs, so avoid making scheduling latency look like an
                // approval behavior failure.
                let event = shell_event(
                    call_id,
                    command,
                    /*timeout_ms*/ 30_000,
                    sandbox_permissions,
                )?;
                Ok((event, Some(command.to_string())))
            }
            ActionKind::RunCommandWithPolicy { command, .. } => {
                // Bazel Linux runners can be heavily oversubscribed while this
                // matrix runs, so avoid making scheduling latency look like an
                // approval behavior failure.
                let event = shell_event(
                    call_id,
                    command,
                    /*timeout_ms*/ 30_000,
                    sandbox_permissions,
                )?;
                Ok((event, Some(command.to_string())))
            }
            ActionKind::RunCommandWithPrefixRule {
                command,
                prefix_rule,
            } => {
                let event = shell_event_with_prefix_rule(
                    call_id,
                    command,
                    /*timeout_ms*/ 30_000,
                    sandbox_permissions,
                    Some(prefix_rule.iter().map(|part| (*part).to_string()).collect()),
                )?;
                Ok((event, Some(command.to_string())))
            }
            ActionKind::RunUnifiedExecCommand {
                command,
                justification,
            } => {
                let event = exec_command_event(
                    call_id,
                    command,
                    Some(1000),
                    sandbox_permissions,
                    *justification,
                )?;
                Ok((event, Some(command.to_string())))
            }
            ActionKind::ApplyPatchFreeform { target, content } => {
                let (path, patch_path) = target.resolve_for_patch(test);
                let _ = fs::remove_file(&path);
                let patch = build_add_file_patch(&patch_path, content);
                Ok((ev_apply_patch_custom_tool_call(call_id, &patch), None))
            }
            ActionKind::ApplyPatchShell { target, content } => {
                let (path, patch_path) = target.resolve_for_patch(test);
                let _ = fs::remove_file(&path);
                let patch = build_add_file_patch(&patch_path, content);
                let command = shell_apply_patch_command(&patch);
                // Bazel may need to launch the configured Codex helper binary
                // to apply the verified patch, which can exceed the normal
                // short command timeout on slower CI runners.
                let timeout_ms = 30_000;
                let event = shell_event(call_id, &command, timeout_ms, sandbox_permissions)?;
                Ok((event, Some(command)))
            }
        }
    }
}

fn build_add_file_patch(patch_path: &str, content: &str) -> String {
    format!("*** Begin Patch\n*** Add File: {patch_path}\n+{content}\n*** End Patch\n")
}

fn shell_apply_patch_command(patch: &str) -> String {
    let mut script = String::from("apply_patch <<'PATCH'\n");
    script.push_str(patch);
    if !patch.ends_with('\n') {
        script.push('\n');
    }
    script.push_str("PATCH\n");
    script
}

fn shell_event(
    call_id: &str,
    command: &str,
    timeout_ms: u64,
    sandbox_permissions: SandboxPermissions,
) -> Result<Value> {
    shell_event_with_prefix_rule(
        call_id,
        command,
        timeout_ms,
        sandbox_permissions,
        /*prefix_rule*/ None,
    )
}

fn shell_event_with_prefix_rule(
    call_id: &str,
    command: &str,
    timeout_ms: u64,
    sandbox_permissions: SandboxPermissions,
    prefix_rule: Option<Vec<String>>,
) -> Result<Value> {
    let mut args = json!({
        "cmd": command,
        "yield_time_ms": timeout_ms,
    });
    if sandbox_permissions.requests_sandbox_override() {
        args["sandbox_permissions"] = json!(sandbox_permissions);
        args["justification"] = json!(DEFAULT_UNIFIED_EXEC_JUSTIFICATION);
    }
    if let Some(prefix_rule) = prefix_rule {
        args["prefix_rule"] = json!(prefix_rule);
    }
    let args_str = serde_json::to_string(&args)?;
    Ok(ev_function_call(call_id, "exec_command", &args_str))
}

fn exec_command_event(
    call_id: &str,
    cmd: &str,
    yield_time_ms: Option<u64>,
    sandbox_permissions: SandboxPermissions,
    justification: Option<&str>,
) -> Result<Value> {
    let mut args = json!({
        "cmd": cmd.to_string(),
    });
    if let Some(yield_time_ms) = yield_time_ms {
        args["yield_time_ms"] = json!(yield_time_ms);
    }
    if sandbox_permissions.requests_sandbox_override() {
        args["sandbox_permissions"] = json!(sandbox_permissions);
        let reason = justification.unwrap_or(DEFAULT_UNIFIED_EXEC_JUSTIFICATION);
        args["justification"] = json!(reason);
    }
    let args_str = serde_json::to_string(&args)?;
    Ok(ev_function_call(call_id, "exec_command", &args_str))
}

#[derive(Clone)]
enum Expectation {
    FileCreatedNoExitCode {
        target: TargetPath,
        content: &'static str,
    },
    PatchApplied {
        target: TargetPath,
        content: &'static str,
    },
    FileNotCreated {
        target: TargetPath,
        message_contains: &'static [&'static str],
    },
    NetworkSuccessNoExitCode {
        body_contains: &'static str,
    },
    NetworkFailure {
        expect_tag: &'static str,
    },
    CommandSuccessNoExitCode {
        stdout_contains: &'static str,
    },
    CommandFailure {
        output_contains: &'static str,
    },
}

impl Expectation {
    fn verify(&self, test: &TestCodex, result: &CommandResult) -> Result<()> {
        match self {
            Expectation::FileCreatedNoExitCode { target, content } => {
                let (path, _) = target.resolve_for_patch(test);
                assert!(
                    result.exit_code.is_none() || result.exit_code == Some(0),
                    "expected no exit code for {path:?}",
                );
                assert!(
                    result.stdout.contains(content),
                    "stdout missing {content:?}: {}",
                    result.stdout
                );
                let file_contents = fs::read_to_string(&path)?;
                assert!(
                    file_contents.contains(content),
                    "file contents missing {content:?}: {file_contents}"
                );
                let _ = fs::remove_file(path);
            }
            Expectation::PatchApplied { target, content } => {
                let (path, _) = target.resolve_for_patch(test);
                match result.exit_code {
                    Some(0) | None => {
                        if result.exit_code.is_none() {
                            assert!(
                                result.stdout.contains("Success."),
                                "patch output missing success indicator: {}",
                                result.stdout
                            );
                        }
                    }
                    Some(code) => panic!(
                        "expected successful patch exit for {:?}, got {code} with stdout {}",
                        path, result.stdout
                    ),
                }
                let file_contents = fs::read_to_string(&path)?;
                assert!(
                    file_contents.contains(content),
                    "patched file missing {content:?}: {file_contents}"
                );
                let _ = fs::remove_file(path);
            }
            Expectation::FileNotCreated {
                target,
                message_contains,
            } => {
                let (path, _) = target.resolve_for_patch(test);
                assert_ne!(
                    result.exit_code,
                    Some(0),
                    "expected non-zero exit for {path:?}"
                );
                for needle in *message_contains {
                    if needle.contains('|') {
                        let options: Vec<&str> = needle.split('|').collect();
                        let matches_any =
                            options.iter().any(|option| result.stdout.contains(option));
                        assert!(
                            matches_any,
                            "stdout missing one of {options:?}: {}",
                            result.stdout
                        );
                    } else {
                        assert!(
                            result.stdout.contains(needle),
                            "stdout missing {needle:?}: {}",
                            result.stdout
                        );
                    }
                }
                assert!(
                    !path.exists(),
                    "command should not create {path:?}, but file exists"
                );
            }
            Expectation::NetworkSuccessNoExitCode { body_contains } => {
                assert!(
                    result.exit_code.is_none() || result.exit_code == Some(0),
                    "expected no exit code for successful network call: {}",
                    result.stdout
                );
                assert!(
                    result.stdout.contains("OK:"),
                    "stdout missing OK prefix: {}",
                    result.stdout
                );
                assert!(
                    result.stdout.contains(body_contains),
                    "stdout missing body text {body_contains:?}: {}",
                    result.stdout
                );
            }
            Expectation::NetworkFailure { expect_tag } => {
                assert_ne!(
                    result.exit_code,
                    Some(0),
                    "expected non-zero exit for network failure: {}",
                    result.stdout
                );
                assert!(
                    result.stdout.contains("ERR:"),
                    "stdout missing ERR prefix: {}",
                    result.stdout
                );
                assert!(
                    result.stdout.contains(expect_tag),
                    "stdout missing expected tag {expect_tag:?}: {}",
                    result.stdout
                );
            }
            Expectation::CommandSuccessNoExitCode { stdout_contains } => {
                assert!(
                    result.exit_code.is_none() || result.exit_code == Some(0),
                    "expected no exit code for command: {}",
                    result.stdout
                );
                assert!(
                    result.stdout.contains(stdout_contains),
                    "command stdout missing {stdout_contains:?}: {}",
                    result.stdout
                );
            }
            Expectation::CommandFailure { output_contains } => {
                assert_ne!(
                    result.exit_code,
                    Some(0),
                    "expected non-zero exit for command failure: {}",
                    result.stdout
                );
                assert!(
                    result.stdout.contains(output_contains),
                    "command failure stderr missing {output_contains:?}: {}",
                    result.stdout
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
enum ExpectedExecPolicyAmendment {
    Prefix(&'static [&'static str]),
    FullCommand,
}

#[derive(Clone)]
enum Outcome {
    Auto,
    ExecApproval {
        decision: ReviewDecision,
        expected_reason: Option<&'static str>,
    },
    ExecApprovalWithAmendment {
        decision: ReviewDecision,
        expected_reason: Option<&'static str>,
        expected_execpolicy_amendment: Option<ExpectedExecPolicyAmendment>,
    },
    PatchApproval {
        decision: ReviewDecision,
        expected_reason: Option<&'static str>,
    },
}

#[derive(Clone)]
struct ScenarioSpec {
    name: &'static str,
    approval_policy: AskForApproval,
    sandbox_policy: SandboxPolicy,
    action: ActionKind,
    sandbox_permissions: SandboxPermissions,
    features: Vec<Feature>,
    outcome: Outcome,
    expectation: Expectation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScenarioGroup {
    DangerFullAccess,
    ReadOnly,
    WorkspaceWrite,
    ApplyPatch,
    UnifiedExec,
}

#[derive(Debug, Eq, PartialEq)]
struct CommandResult {
    exit_code: Option<i64>,
    stdout: String,
}

async fn submit_turn(
    test: &TestCodex,
    prompt: &str,
    approval_policy: AskForApproval,
    sandbox_policy: SandboxPolicy,
) -> Result<()> {
    let session_model = test.session_configured.model.clone();

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: prompt.into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                environments: Some(local_requests(test.config.cwd.clone())),
                approval_policy: Some(approval_policy),
                approvals_reviewer: Some(ApprovalsReviewer::User),
                sandbox_policy: Some(sandbox_policy),
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Default,
                    settings: Settings {
                        model: session_model,
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            }),
        )
        .await?;

    Ok(())
}

async fn submit_turn_preserving_active_permission_profile(
    test: &TestCodex,
    prompt: &str,
    approval_policy: AskForApproval,
) -> Result<()> {
    let session_model = test.session_configured.model.clone();

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: prompt.into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                environments: Some(local_requests(test.config.cwd.clone())),
                approval_policy: Some(approval_policy),
                approvals_reviewer: Some(ApprovalsReviewer::User),
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Default,
                    settings: Settings {
                        model: session_model,
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            }),
        )
        .await?;

    Ok(())
}

fn assert_active_workspace_permission_profile(test: &TestCodex) {
    assert_eq!(
        test.session_configured
            .active_permission_profile
            .as_ref()
            .map(|profile| profile.id.as_str()),
        Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE)
    );
}

fn parse_result(item: &Value) -> CommandResult {
    let output_str = item
        .get("output")
        .and_then(Value::as_str)
        .expect("shell output payload");
    if output_str == "Completed" {
        return CommandResult {
            exit_code: Some(0),
            stdout: String::new(),
        };
    }
    let (header, stdout) = output_str.split_once('\n').unwrap_or((output_str, ""));
    if let Some(exit_code) = header
        .strip_prefix("Exit ")
        .and_then(|exit| exit.parse().ok())
    {
        return CommandResult {
            exit_code: Some(exit_code),
            stdout: stdout.to_string(),
        };
    }
    match serde_json::from_str::<Value>(output_str) {
        Ok(parsed) => {
            let exit_code = parsed["metadata"]["exit_code"].as_i64();
            let stdout = parsed["output"].as_str().unwrap_or_default().to_string();
            CommandResult { exit_code, stdout }
        }
        Err(_) => {
            let structured = Regex::new(r"(?s)^Exit code:\s*(-?\d+).*?Output:\n(.*)$").unwrap();
            let regex =
                Regex::new(r"(?s)^.*?Process exited with code (\d+)\n.*?Output:\n(.*)$").unwrap();
            // parse freeform output
            if let Some(captures) = structured.captures(output_str) {
                let exit_code = captures.get(1).unwrap().as_str().parse::<i64>().unwrap();
                let output = captures.get(2).unwrap().as_str();
                CommandResult {
                    exit_code: Some(exit_code),
                    stdout: output.to_string(),
                }
            } else if let Some(captures) = regex.captures(output_str) {
                let exit_code = captures.get(1).unwrap().as_str().parse::<i64>().unwrap();
                let output = captures.get(2).unwrap().as_str();
                CommandResult {
                    exit_code: Some(exit_code),
                    stdout: output.to_string(),
                }
            } else {
                CommandResult {
                    exit_code: None,
                    stdout: output_str.to_string(),
                }
            }
        }
    }
}

async fn expect_exec_approval(
    test: &TestCodex,
    expected_command: &str,
) -> ExecApprovalRequestEvent {
    let event = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ExecApprovalRequest(_) | EventMsg::TurnComplete(_)
        )
    })
    .await;

    match event {
        EventMsg::ExecApprovalRequest(approval) => {
            let last_arg = approval
                .command
                .last()
                .map(std::string::String::as_str)
                .unwrap_or_default();
            assert_eq!(last_arg, expected_command);
            approval
        }
        EventMsg::TurnComplete(_) => panic!("expected approval request before completion"),
        other => panic!("unexpected event: {other:?}"),
    }
}

async fn expect_patch_approval(
    test: &TestCodex,
    expected_call_id: &str,
) -> ApplyPatchApprovalRequestEvent {
    let event = wait_for_event_with_timeout(
        &test.codex,
        |event| {
            matches!(
                event,
                EventMsg::ApplyPatchApprovalRequest(_)
                    | EventMsg::ExecApprovalRequest(_)
                    | EventMsg::Error(_)
                    | EventMsg::TurnAborted(_)
                    | EventMsg::ShutdownComplete
                    | EventMsg::TurnComplete(_)
            )
        },
        Duration::from_secs(15),
    )
    .await;

    match event {
        EventMsg::ApplyPatchApprovalRequest(approval) => {
            assert_eq!(approval.call_id, expected_call_id);
            approval
        }
        EventMsg::TurnComplete(_) => panic!("expected patch approval request before completion"),
        other => panic!("unexpected event: {other:?}"),
    }
}

async fn wait_for_completion_without_approval(test: &TestCodex) {
    let event = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ExecApprovalRequest(_) | EventMsg::TurnComplete(_)
        )
    })
    .await;

    match event {
        EventMsg::TurnComplete(_) => {}
        EventMsg::ExecApprovalRequest(event) => {
            panic!("unexpected approval request: {:?}", event.command)
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

async fn wait_for_completion(test: &TestCodex) {
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
}

fn body_contains(req: &Request, text: &str) -> bool {
    let is_zstd = req
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
        });
    let bytes = if is_zstd {
        zstd::stream::decode_all(std::io::Cursor::new(&req.body)).ok()
    } else {
        Some(req.body.clone())
    };
    bytes
        .and_then(|body| String::from_utf8(body).ok())
        .is_some_and(|body| body.contains(text))
}

async fn wait_for_spawned_thread(test: &TestCodex) -> Result<Arc<CodexThread>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let ids = test.thread_manager.list_thread_ids().await;
        if let Some(thread_id) = ids
            .iter()
            .find(|id| **id != test.session_configured.thread_id)
        {
            return test
                .thread_manager
                .get_thread(*thread_id)
                .await
                .map_err(anyhow::Error::from);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for spawned thread");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn scenarios() -> Vec<ScenarioSpec> {
    use AskForApproval::*;

    let workspace_write = |network_access| SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![],
        network_access,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };

    vec![
        ScenarioSpec {
            name: "danger_full_access_on_request_allows_outside_write",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::WriteFile {
                target: TargetPath::OutsideWorkspace("dfa_on_request.txt"),
                content: "danger-on-request",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::OutsideWorkspace("dfa_on_request.txt"),
                content: "danger-on-request",
            },
        },
        ScenarioSpec {
            name: "danger_full_access_on_request_allows_outside_write_gpt_5_1_no_exit",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::WriteFile {
                target: TargetPath::OutsideWorkspace("dfa_on_request_5_1.txt"),
                content: "danger-on-request",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::OutsideWorkspace("dfa_on_request_5_1.txt"),
                content: "danger-on-request",
            },
        },
        ScenarioSpec {
            name: "danger_full_access_on_request_allows_network",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::FetchUrlNoProxy {
                endpoint: "/dfa/network",
                response_body: "danger-network-ok",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::NetworkSuccessNoExitCode {
                body_contains: "danger-network-ok",
            },
        },
        ScenarioSpec {
            name: "danger_full_access_on_request_allows_network_gpt_5_1_no_exit",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::FetchUrlNoProxy {
                endpoint: "/dfa/network",
                response_body: "danger-network-ok",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::NetworkSuccessNoExitCode {
                body_contains: "danger-network-ok",
            },
        },
        ScenarioSpec {
            name: "simple_command_unless_trusted_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::RunCommand {
                command: "echo trusted-unless",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("blocked in untrusted project"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "blocked in untrusted project",
            },
        },
        ScenarioSpec {
            name: "simple_command_unless_trusted_requires_approval_gpt_5_1_no_exit",
            approval_policy: UnlessTrusted,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::RunCommand {
                command: "echo trusted-unless",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("blocked in untrusted project"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "blocked in untrusted project",
            },
        },
        ScenarioSpec {
            name: "cat_redirect_unless_trusted_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: r#"cat < "hello" > /var/test.txt"#,
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("blocked by distinctive approval policy"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "blocked by distinctive approval policy",
            },
        },
        ScenarioSpec {
            name: "cat_redirect_on_request_requires_approval",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: r#"cat < "hello" > /var/test.txt"#,
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        ScenarioSpec {
            name: "simple_command_escalation_on_request_requires_approval",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: "echo known-safe-escalation",
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApprovalWithAmendment {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
                expected_execpolicy_amendment: Some(ExpectedExecPolicyAmendment::Prefix(&[
                    "echo",
                    "known-safe-escalation",
                ])),
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        ScenarioSpec {
            name: "simple_command_escalation_granular_sandbox_enabled_requires_approval",
            approval_policy: Granular(GranularApprovalConfig {
                sandbox_approval: true,
                rules: true,
                skill_approval: true,
                request_permissions: true,
                mcp_elicitations: true,
            }),
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: "echo known-safe-escalation",
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApprovalWithAmendment {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
                expected_execpolicy_amendment: Some(ExpectedExecPolicyAmendment::Prefix(&[
                    "echo",
                    "known-safe-escalation",
                ])),
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        ScenarioSpec {
            name: "simple_command_escalation_granular_sandbox_disabled_rejects",
            approval_policy: Granular(GranularApprovalConfig {
                sandbox_approval: false,
                rules: true,
                skill_approval: true,
                request_permissions: true,
                mcp_elicitations: true,
            }),
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: "echo known-safe-escalation-granular-disabled",
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::CommandFailure {
                output_contains: "you cannot ask for escalated permissions",
            },
        },
        ScenarioSpec {
            name: "cat_heredoc_inner_allow_rule_requires_escalation_approval",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommandWithPolicy {
                command: "cat <<'EOF'\nhello\nEOF",
                policy_src: r#"prefix_rule(pattern=["cat"], decision="allow")"#,
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApprovalWithAmendment {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
                expected_execpolicy_amendment: Some(ExpectedExecPolicyAmendment::FullCommand),
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        ScenarioSpec {
            name: "cat_heredoc_file_redirect_prefix_rule_requires_escalation_approval",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommandWithPrefixRule {
                command: r#"cat <<'EOF' > /tmp/out.txt
                hello
                EOF"#,
                prefix_rule: &["cat"],
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        ScenarioSpec {
            name: "cat_heredoc_variable_assignment_policy_requires_escalation_approval",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommandWithPolicy {
                command: r#"PATH=/tmp/evil:$PATH cat <<'EOF'
                hello
                EOF"#,
                policy_src: r#"prefix_rule(pattern=["cat"], decision="allow")"#,
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        ScenarioSpec {
            name: "python_heredoc_requested_prefix_rule_proposes_full_command",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommandWithPrefixRule {
                command: r#"python3 <<'PY'
                print('hello')
                PY"#,
                prefix_rule: &["python3"],
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApprovalWithAmendment {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
                expected_execpolicy_amendment: Some(ExpectedExecPolicyAmendment::FullCommand),
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        ScenarioSpec {
            name: "danger_full_access_unless_trusted_requests_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::WriteFile {
                target: TargetPath::OutsideWorkspace("dfa_unless_trusted.txt"),
                content: "danger-unless-trusted",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::OutsideWorkspace("dfa_unless_trusted.txt"),
                content: "danger-unless-trusted",
            },
        },
        ScenarioSpec {
            name: "danger_full_access_unless_trusted_requests_approval_gpt_5_1_no_exit",
            approval_policy: UnlessTrusted,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::WriteFile {
                target: TargetPath::OutsideWorkspace("dfa_unless_trusted_5_1.txt"),
                content: "danger-unless-trusted",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::OutsideWorkspace("dfa_unless_trusted_5_1.txt"),
                content: "danger-unless-trusted",
            },
        },
        ScenarioSpec {
            name: "danger_full_access_never_allows_outside_write",
            approval_policy: Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::WriteFile {
                target: TargetPath::OutsideWorkspace("dfa_never.txt"),
                content: "danger-never",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::OutsideWorkspace("dfa_never.txt"),
                content: "danger-never",
            },
        },
        ScenarioSpec {
            name: "danger_full_access_never_allows_outside_write_gpt_5_1_no_exit",
            approval_policy: Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::WriteFile {
                target: TargetPath::OutsideWorkspace("dfa_never_5_1.txt"),
                content: "danger-never",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::OutsideWorkspace("dfa_never_5_1.txt"),
                content: "danger-never",
            },
        },
        ScenarioSpec {
            name: "read_only_on_request_requires_approval",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::WriteFile {
                target: TargetPath::Workspace("ro_on_request.txt"),
                content: "read-only-approval",
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::Workspace("ro_on_request.txt"),
                content: "read-only-approval",
            },
        },
        ScenarioSpec {
            name: "read_only_on_request_requires_approval_gpt_5_1_no_exit",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::WriteFile {
                target: TargetPath::Workspace("ro_on_request_5_1.txt"),
                content: "read-only-approval",
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::Workspace("ro_on_request_5_1.txt"),
                content: "read-only-approval",
            },
        },
        ScenarioSpec {
            name: "simple_command_on_request_read_only_runs_without_prompt",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::RunCommand {
                command: "echo trusted-read-only",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::CommandSuccessNoExitCode {
                stdout_contains: "trusted-read-only",
            },
        },
        ScenarioSpec {
            name: "simple_command_on_request_read_only_runs_without_prompt_gpt_5_1_no_exit",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::RunCommand {
                command: "echo trusted-read-only",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::CommandSuccessNoExitCode {
                stdout_contains: "trusted-read-only",
            },
        },
        ScenarioSpec {
            name: "read_only_on_request_blocks_network",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::FetchUrl {
                endpoint: "/ro/network-blocked",
                response_body: "should-not-see",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::NetworkFailure { expect_tag: "ERR:" },
        },
        ScenarioSpec {
            name: "read_only_on_request_denied_blocks_execution",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::WriteFile {
                target: TargetPath::Workspace("ro_on_request_denied.txt"),
                content: "should-not-write",
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
            },
            expectation: Expectation::FileNotCreated {
                target: TargetPath::Workspace("ro_on_request_denied.txt"),
                message_contains: &["rejected by user"],
            },
        },
        ScenarioSpec {
            name: "read_only_on_request_network_escalates_when_approved",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::FetchUrl {
                endpoint: "/ro/network-approved",
                response_body: "read-only-network-ok",
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::NetworkSuccessNoExitCode {
                body_contains: "read-only-network-ok",
            },
        },
        ScenarioSpec {
            name: "read_only_on_request_network_escalates_when_approved_gpt_5_1_no_exit",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::FetchUrl {
                endpoint: "/ro/network-approved",
                response_body: "read-only-network-ok",
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::NetworkSuccessNoExitCode {
                body_contains: "read-only-network-ok",
            },
        },
        ScenarioSpec {
            name: "apply_patch_exec_command_requires_patch_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::ApplyPatchShell {
                target: TargetPath::Workspace("apply_patch_shell.txt"),
                content: "shell-apply-patch",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::PatchApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::PatchApplied {
                target: TargetPath::Workspace("apply_patch_shell.txt"),
                content: "shell-apply-patch",
            },
        },
        ScenarioSpec {
            name: "apply_patch_freeform_auto_inside_workspace",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::ApplyPatchFreeform {
                target: TargetPath::Workspace("apply_patch_freeform.txt"),
                content: "freeform-apply-patch",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::PatchApplied {
                target: TargetPath::Workspace("apply_patch_freeform.txt"),
                content: "freeform-apply-patch",
            },
        },
        ScenarioSpec {
            name: "apply_patch_freeform_danger_allows_outside_workspace",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::ApplyPatchFreeform {
                target: TargetPath::OutsideWorkspace("apply_patch_freeform_danger.txt"),
                content: "freeform-patch-danger",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::PatchApplied {
                target: TargetPath::OutsideWorkspace("apply_patch_freeform_danger.txt"),
                content: "freeform-patch-danger",
            },
        },
        ScenarioSpec {
            name: "apply_patch_freeform_outside_requires_patch_approval",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::ApplyPatchFreeform {
                target: TargetPath::OutsideWorkspace("apply_patch_freeform_outside.txt"),
                content: "freeform-patch-outside",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::PatchApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::PatchApplied {
                target: TargetPath::OutsideWorkspace("apply_patch_freeform_outside.txt"),
                content: "freeform-patch-outside",
            },
        },
        ScenarioSpec {
            name: "apply_patch_freeform_outside_denied_blocks_patch",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::ApplyPatchFreeform {
                target: TargetPath::OutsideWorkspace("apply_patch_freeform_outside_denied.txt"),
                content: "freeform-patch-outside-denied",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::PatchApproval {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
            },
            expectation: Expectation::FileNotCreated {
                target: TargetPath::OutsideWorkspace("apply_patch_freeform_outside_denied.txt"),
                message_contains: &["patch rejected by user"],
            },
        },
        ScenarioSpec {
            name: "apply_patch_exec_command_outside_requires_patch_approval",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::ApplyPatchShell {
                target: TargetPath::OutsideWorkspace("apply_patch_shell_outside.txt"),
                content: "shell-patch-outside",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::PatchApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::PatchApplied {
                target: TargetPath::OutsideWorkspace("apply_patch_shell_outside.txt"),
                content: "shell-patch-outside",
            },
        },
        ScenarioSpec {
            name: "apply_patch_freeform_unless_trusted_requires_patch_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::ApplyPatchFreeform {
                target: TargetPath::Workspace("apply_patch_freeform_unless_trusted.txt"),
                content: "freeform-patch-unless-trusted",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::PatchApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::PatchApplied {
                target: TargetPath::Workspace("apply_patch_freeform_unless_trusted.txt"),
                content: "freeform-patch-unless-trusted",
            },
        },
        ScenarioSpec {
            name: "apply_patch_freeform_never_rejects_outside_workspace",
            approval_policy: Never,
            sandbox_policy: workspace_write(false),
            action: ActionKind::ApplyPatchFreeform {
                target: TargetPath::OutsideWorkspace("apply_patch_freeform_never.txt"),
                content: "freeform-patch-never",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::FileNotCreated {
                target: TargetPath::OutsideWorkspace("apply_patch_freeform_never.txt"),
                message_contains: &[
                    "patch rejected: writing outside of the project; rejected by user approval settings",
                ],
            },
        },
        ScenarioSpec {
            name: "read_only_never_reports_sandbox_failure",
            approval_policy: Never,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::WriteFile {
                target: TargetPath::Workspace("ro_never.txt"),
                content: "read-only-never",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::FileNotCreated {
                target: TargetPath::Workspace("ro_never.txt"),
                message_contains: if cfg!(target_os = "linux") {
                    &["Permission denied|Read-only file system"]
                } else {
                    &[
                        "Permission denied|Operation not permitted|operation not permitted|\
                         Read-only file system",
                    ]
                },
            },
        },
        ScenarioSpec {
            name: "simple_command_never_runs_without_prompt",
            approval_policy: Never,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::RunCommand {
                command: "echo trusted-never",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::CommandSuccessNoExitCode {
                stdout_contains: "trusted-never",
            },
        },
        ScenarioSpec {
            name: "workspace_write_on_request_allows_workspace_write",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::WriteFile {
                target: TargetPath::Workspace("ww_on_request.txt"),
                content: "workspace-on-request",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::Workspace("ww_on_request.txt"),
                content: "workspace-on-request",
            },
        },
        ScenarioSpec {
            name: "workspace_write_network_disabled_blocks_network",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::FetchUrl {
                endpoint: "/ww/network-blocked",
                response_body: "workspace-network-blocked",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::NetworkFailure { expect_tag: "ERR:" },
        },
        ScenarioSpec {
            name: "workspace_write_on_request_requires_approval_outside_workspace",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::WriteFile {
                target: TargetPath::OutsideWorkspace("ww_on_request_outside.txt"),
                content: "workspace-on-request-outside",
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::Approved,
                expected_reason: None,
            },
            expectation: Expectation::FileCreatedNoExitCode {
                target: TargetPath::OutsideWorkspace("ww_on_request_outside.txt"),
                content: "workspace-on-request-outside",
            },
        },
        ScenarioSpec {
            name: "workspace_write_network_enabled_allows_network",
            approval_policy: OnRequest,
            sandbox_policy: workspace_write(true),
            action: ActionKind::FetchUrl {
                endpoint: "/ww/network-ok",
                response_body: "workspace-network-ok",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::NetworkSuccessNoExitCode {
                body_contains: "workspace-network-ok",
            },
        },
        ScenarioSpec {
            name: "workspace_write_never_blocks_outside_workspace",
            approval_policy: Never,
            sandbox_policy: workspace_write(false),
            action: ActionKind::WriteFile {
                target: TargetPath::OutsideWorkspace("ww_never.txt"),
                content: "workspace-never",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::Auto,
            expectation: Expectation::FileNotCreated {
                target: TargetPath::OutsideWorkspace("ww_never.txt"),
                message_contains: if cfg!(target_os = "linux") {
                    &["Permission denied|Read-only file system"]
                } else {
                    &[
                        "Permission denied|Operation not permitted|operation not permitted|\
                         Read-only file system",
                    ]
                },
            },
        },
        ScenarioSpec {
            name: "unified exec on request no approval for simple command",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::RunUnifiedExecCommand {
                command: "echo \"hello unified exec\"",
                justification: None,
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![Feature::UnifiedExec],
            outcome: Outcome::Auto,
            expectation: Expectation::CommandSuccessNoExitCode {
                stdout_contains: "hello unified exec",
            },
        },
        #[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
        // Linux sandbox arg0 test workaround doesn't work on ARM
        ScenarioSpec {
            name: "unified exec on request escalated requires approval",
            approval_policy: OnRequest,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            action: ActionKind::RunUnifiedExecCommand {
                command: "python3 -c 'print('\"'\"'escalated unified exec'\"'\"')'",
                justification: Some(DEFAULT_UNIFIED_EXEC_JUSTIFICATION),
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![Feature::UnifiedExec],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::Approved,
                expected_reason: Some(DEFAULT_UNIFIED_EXEC_JUSTIFICATION),
            },
            expectation: Expectation::CommandSuccessNoExitCode {
                stdout_contains: "escalated unified exec",
            },
        },
        ScenarioSpec {
            name: "unified exec on request requires approval unless trusted",
            approval_policy: AskForApproval::UnlessTrusted,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            action: ActionKind::RunUnifiedExecCommand {
                command: "git reset --hard",
                justification: None,
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![Feature::UnifiedExec],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        ScenarioSpec {
            name: "heredoc with redirect still requires approval",
            approval_policy: AskForApproval::OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunUnifiedExecCommand {
                command: "cat <<'EOF' > /tmp/out.txt \nhello\nEOF",
                justification: None,
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![Feature::UnifiedExec],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        ScenarioSpec {
            name: "compound command with one safe command still requires approval",
            approval_policy: AskForApproval::OnRequest,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunUnifiedExecCommand {
                command: "cat ./one.txt && touch ./two.txt",
                justification: None,
            },
            sandbox_permissions: SandboxPermissions::RequireEscalated,
            features: vec![Feature::UnifiedExec],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected by user"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected by user",
            },
        },
        // Deny these commands before execution. The nonexistent find target also
        // keeps a regression from deleting anything in the test workspace.
        ScenarioSpec {
            name: "find_brace_expansion_unless_trusted_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: "find ./missing-approval-target -{delete,print}",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected dynamic shell word"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected dynamic shell word",
            },
        },
        ScenarioSpec {
            name: "rg_brace_expansion_unless_trusted_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: "rg --pre{=,=sh} pattern missing-approval-payload.sh",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected dynamic shell word"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected dynamic shell word",
            },
        },
        ScenarioSpec {
            name: "brace_expansion_literal_allow_rule_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommandWithPolicy {
                command: "find ./missing-approval-target -{delete,print}",
                policy_src: r#"prefix_rule(pattern=["find", "./missing-approval-target", "-{delete,print}"], decision="allow")"#,
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected dynamic shell word"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected dynamic shell word",
            },
        },
        ScenarioSpec {
            name: "rg_brace_expansion_literal_allow_rule_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommandWithPolicy {
                command: "rg --pre{=,=sh} pattern missing-approval-payload.sh",
                policy_src: r#"prefix_rule(pattern=["rg", "--pre{=,=sh}"], decision="allow")"#,
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected dynamic shell word"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected dynamic shell word",
            },
        },
        ScenarioSpec {
            name: "find_glob_unless_trusted_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: "find ./missing-approval-target -del*",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected dynamic shell word"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected dynamic shell word",
            },
        },
        ScenarioSpec {
            name: "find_escape_unless_trusted_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: r"find ./missing-approval-target -de\lete",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected dynamic shell word"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected dynamic shell word",
            },
        },
        ScenarioSpec {
            name: "find_quoted_escape_unless_trusted_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: "find ./missing-approval-target \"-de\\\nlete\"",
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected dynamic shell word"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected dynamic shell word",
            },
        },
        ScenarioSpec {
            name: "heredoc_glob_literal_allow_rule_requires_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommandWithPolicy {
                command: "find ./missing-approval-target -del* <<'EOF'\nEOF",
                policy_src: r#"prefix_rule(pattern=["find", "./missing-approval-target", "-del*"], decision="allow")"#,
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("rejected dynamic shell word"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "rejected dynamic shell word",
            },
        },
        ScenarioSpec {
            name: "quoted_shell_metacharacters_unless_trusted_require_approval",
            approval_policy: UnlessTrusted,
            sandbox_policy: workspace_write(false),
            action: ActionKind::RunCommand {
                command: r#"echo -g"*.py" '-{delete,print}'"#,
            },
            sandbox_permissions: SandboxPermissions::UseDefault,
            features: vec![],
            outcome: Outcome::ExecApproval {
                decision: ReviewDecision::denied("blocked in untrusted project"),
                expected_reason: None,
            },
            expectation: Expectation::CommandFailure {
                output_contains: "blocked in untrusted project",
            },
        },
    ]
}

#[test_case(ScenarioGroup::DangerFullAccess ; "danger_full_access")]
#[test_case(ScenarioGroup::ReadOnly ; "read_only")]
#[test_case(ScenarioGroup::WorkspaceWrite ; "workspace_write")]
#[test_case(ScenarioGroup::ApplyPatch ; "apply_patch")]
#[test_case(ScenarioGroup::UnifiedExec ; "unified_exec")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_matrix_covers_group(group: ScenarioGroup) -> Result<()> {
    run_scenario_group(group).await
}

async fn run_scenario_group(group: ScenarioGroup) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let scenarios = scenarios()
        .into_iter()
        .filter(|scenario| scenario_group(scenario) == group)
        .collect::<Vec<_>>();
    assert!(!scenarios.is_empty(), "expected scenarios for {group:?}");

    for scenario in scenarios {
        run_scenario(&scenario)
            .await
            .with_context(|| format!("approval scenario failed: {}", scenario.name))?;
    }

    Ok(())
}

fn scenario_group(scenario: &ScenarioSpec) -> ScenarioGroup {
    match &scenario.action {
        ActionKind::ApplyPatchFreeform { .. } | ActionKind::ApplyPatchShell { .. } => {
            ScenarioGroup::ApplyPatch
        }
        ActionKind::RunUnifiedExecCommand { .. } => ScenarioGroup::UnifiedExec,
        ActionKind::WriteFile { .. }
        | ActionKind::FetchUrlNoProxy { .. }
        | ActionKind::FetchUrl { .. }
        | ActionKind::RunCommand { .. }
        | ActionKind::RunCommandWithPolicy { .. }
        | ActionKind::RunCommandWithPrefixRule { .. } => match &scenario.sandbox_policy {
            SandboxPolicy::DangerFullAccess => ScenarioGroup::DangerFullAccess,
            SandboxPolicy::ReadOnly { .. } => ScenarioGroup::ReadOnly,
            SandboxPolicy::WorkspaceWrite { .. } => ScenarioGroup::WorkspaceWrite,
            SandboxPolicy::ExternalSandbox { .. } => ScenarioGroup::WorkspaceWrite,
        },
    }
}

async fn run_scenario(scenario: &ScenarioSpec) -> Result<()> {
    eprintln!("running approval scenario: {}", scenario.name);
    let server = start_mock_server().await;
    let approval_policy = scenario.approval_policy;
    let sandbox_policy = scenario.sandbox_policy.clone();
    let features = scenario.features.clone();
    let policy_src = scenario.action.policy_src();
    let thread_store_id = format!("approval-scenario-{}", scenario.name);

    let mut builder = test_codex()
        .with_model("gpt-5.5")
        .with_config(move |config| {
            // These scenarios assert tool behavior, not rollout persistence.
            config.experimental_thread_store = ThreadStoreConfig::InMemory {
                id: thread_store_id,
            };
            config.permissions.approval_policy = Constrained::allow_any(approval_policy);
            config.workspace_roots_explicit = false;
            let permission_profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                &sandbox_policy,
                config.cwd.as_path(),
            );
            config
                .permissions
                .can_set_permission_profile(&permission_profile)
                .expect("set permission profile");
            config
                .permissions
                .set_workspace_roots(vec![config.cwd.clone()]);
            config
                .permissions
                .set_permission_profile(permission_profile)
                .expect("set permission profile");
            config.workspace_roots = config.permissions.workspace_roots().to_vec();
            for feature in features {
                config
                    .features
                    .enable(feature)
                    .expect("test config should allow feature update");
            }
        });
    if let Some(policy_src) = policy_src {
        builder = builder.with_pre_build_hook(move |home| {
            let rules_dir = home.join("rules");
            fs::create_dir_all(&rules_dir).expect("create rules dir");
            fs::write(rules_dir.join("default.rules"), policy_src).expect("write policy");
        });
    }
    let test = builder.build(&server).await?;

    let call_id = scenario.name;
    let (event, expected_command) = scenario
        .action
        .prepare(&test, &server, call_id, scenario.sandbox_permissions)
        .await?;
    if let Some(command) = expected_command.as_deref() {
        eprintln!("approval scenario {} command: {command}", scenario.name);
    }

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            event,
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let results_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        scenario.name,
        scenario.approval_policy,
        scenario.sandbox_policy.clone(),
    )
    .await?;

    match &scenario.outcome {
        Outcome::Auto => {
            wait_for_completion_without_approval(&test).await;
        }
        Outcome::ExecApproval {
            decision,
            expected_reason,
        } => {
            let command = expected_command
                .as_deref()
                .expect("exec approval requires shell command");
            let approval = expect_exec_approval(&test, command).await;
            if let Some(expected_reason) = expected_reason {
                assert_eq!(
                    approval.reason.as_deref(),
                    Some(*expected_reason),
                    "unexpected approval reason for {}",
                    scenario.name
                );
            }
            test.codex
                .submit(Op::ExecApproval {
                    id: approval.effective_approval_id(),
                    turn_id: None,
                    decision: decision.clone(),
                })
                .await?;
            wait_for_completion(&test).await;
        }
        Outcome::ExecApprovalWithAmendment {
            decision,
            expected_reason,
            expected_execpolicy_amendment,
        } => {
            let command = expected_command
                .as_deref()
                .expect("exec approval requires shell command");
            let approval = expect_exec_approval(&test, command).await;
            if let Some(expected_reason) = expected_reason {
                assert_eq!(
                    approval.reason.as_deref(),
                    Some(*expected_reason),
                    "unexpected approval reason for {}",
                    scenario.name
                );
            }
            let expected_execpolicy_amendment =
                expected_execpolicy_amendment.as_ref().map(|expected| {
                    ExecPolicyAmendment::new(match expected {
                        ExpectedExecPolicyAmendment::Prefix(command) => {
                            command.iter().map(|part| (*part).to_string()).collect()
                        }
                        ExpectedExecPolicyAmendment::FullCommand => approval.command.clone(),
                    })
                });
            assert_eq!(
                approval.proposed_execpolicy_amendment, expected_execpolicy_amendment,
                "unexpected execpolicy amendment for {}",
                scenario.name
            );
            test.codex
                .submit(Op::ExecApproval {
                    id: approval.effective_approval_id(),
                    turn_id: None,
                    decision: decision.clone(),
                })
                .await?;
            wait_for_completion(&test).await;
        }
        Outcome::PatchApproval {
            decision,
            expected_reason,
        } => {
            let approval = expect_patch_approval(&test, call_id).await;
            if let Some(expected_reason) = expected_reason {
                assert_eq!(
                    approval.reason.as_deref(),
                    Some(*expected_reason),
                    "unexpected patch approval reason for {}",
                    scenario.name
                );
            }
            test.codex
                .submit(Op::PatchApproval {
                    id: approval.call_id,
                    decision: decision.clone(),
                })
                .await?;
            wait_for_completion(&test).await;
        }
    }

    let output_request = results_mock.single_request();
    let output_item = if matches!(scenario.action, ActionKind::ApplyPatchFreeform { .. }) {
        output_request.custom_tool_call_output(call_id)
    } else {
        output_request.function_call_output(call_id)
    };
    let result = parse_result(&output_item);
    eprintln!(
        "approval scenario {} result: exit_code={:?} stdout={:?}",
        scenario.name, result.exit_code, result.stdout
    );
    let verification_result = scenario.expectation.verify(&test, &result);
    test.codex.shutdown_and_wait().await?;
    verification_result
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn approving_apply_patch_for_session_skips_future_prompts_for_same_file() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let approval_policy = AskForApproval::UnlessTrusted;
    let sandbox_policy = SandboxPolicy::DangerFullAccess;
    let sandbox_policy_for_config = sandbox_policy.clone();

    let mut builder = test_codex()
        .with_model("gpt-5.5")
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(approval_policy);
            config.workspace_roots_explicit = false;
            let permission_profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                &sandbox_policy_for_config,
                config.cwd.as_path(),
            );
            config
                .permissions
                .can_set_permission_profile(&permission_profile)
                .expect("set permission profile");
            config
                .permissions
                .set_workspace_roots(vec![config.cwd.clone()]);
            config
                .permissions
                .set_permission_profile(permission_profile)
                .expect("set permission profile");
            config.workspace_roots = config.permissions.workspace_roots().to_vec();
            config.approvals_reviewer = ApprovalsReviewer::User;
        });
    let test = builder.build(&server).await?;

    let target = TargetPath::OutsideWorkspace("apply_patch_allow_session.txt");
    let (path, patch_path) = target.resolve_for_patch(&test);
    let _path_cleanup = tempfile::TempPath::try_from_path(path.clone())?;
    let _ = fs::remove_file(&path);

    let patch_add = build_add_file_patch(&patch_path, "before");
    let patch_update = format!(
        "*** Begin Patch\n*** Update File: {patch_path}\n@@\n-before\n+after\n*** End Patch\n"
    );

    let call_id_1 = "apply_patch_allow_session_1";
    let call_id_2 = "apply_patch_allow_session_2";

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_apply_patch_custom_tool_call(call_id_1, &patch_add),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        "apply_patch allow session",
        approval_policy,
        sandbox_policy.clone(),
    )
    .await?;
    let approval = expect_patch_approval(&test, call_id_1).await;
    test.codex
        .submit(Op::PatchApproval {
            id: approval.call_id,
            decision: ReviewDecision::ApprovedForSession,
        })
        .await?;
    wait_for_completion(&test).await;
    assert!(fs::read_to_string(&path)?.contains("before"));

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-3"),
            ev_apply_patch_custom_tool_call(call_id_2, &patch_update),
            ev_completed("resp-3"),
        ]),
    )
    .await;
    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-2", "done"),
            ev_completed("resp-4"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        "apply_patch allow session followup",
        approval_policy,
        sandbox_policy.clone(),
    )
    .await?;

    let event = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ApplyPatchApprovalRequest(_) | EventMsg::TurnComplete(_)
        )
    })
    .await;
    match event {
        EventMsg::TurnComplete(_) => {}
        EventMsg::ApplyPatchApprovalRequest(event) => {
            panic!("unexpected patch approval request: {:?}", event.call_id)
        }
        other => panic!("unexpected event: {other:?}"),
    }

    assert!(fs::read_to_string(&path)?.contains("after"));
    let _ = fs::remove_file(path);

    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn approving_execpolicy_amendment_persists_policy_and_skips_future_prompts() -> Result<()> {
    let server = start_mock_server().await;
    let approval_policy = AskForApproval::UnlessTrusted;
    let sandbox_policy = SandboxPolicy::new_workspace_write_policy();
    let sandbox_policy_for_config = sandbox_policy.clone();
    let mut builder = test_codex().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(approval_policy);
        config.workspace_roots_explicit = false;
        let permission_profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
            &sandbox_policy_for_config,
            config.cwd.as_path(),
        );
        config
            .permissions
            .can_set_permission_profile(&permission_profile)
            .expect("set permission profile");
        config
            .permissions
            .set_workspace_roots(vec![config.cwd.clone()]);
        config
            .permissions
            .set_permission_profile(permission_profile)
            .expect("set permission profile");
        config.workspace_roots = config.permissions.workspace_roots().to_vec();
    });
    let test = builder.build(&server).await?;
    let allow_prefix_path = test.cwd.path().join("allow-prefix.txt");
    let _ = fs::remove_file(&allow_prefix_path);

    let call_id_first = "allow-prefix-first";
    let (first_event, expected_command) = ActionKind::RunCommand {
        command: "touch allow-prefix.txt",
    }
    .prepare(
        &test,
        &server,
        call_id_first,
        SandboxPermissions::UseDefault,
    )
    .await?;
    let expected_command =
        expected_command.expect("execpolicy amendment scenario should produce a shell command");
    let expected_execpolicy_amendment =
        ExecPolicyAmendment::new(vec!["touch".to_string(), "allow-prefix.txt".to_string()]);

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-allow-prefix-1"),
            first_event,
            ev_completed("resp-allow-prefix-1"),
        ]),
    )
    .await;
    let first_results = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-allow-prefix-1", "done"),
            ev_completed("resp-allow-prefix-2"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        "allow-prefix-first",
        approval_policy,
        sandbox_policy.clone(),
    )
    .await?;

    let approval = expect_exec_approval(&test, expected_command.as_str()).await;
    assert_eq!(
        approval.proposed_execpolicy_amendment,
        Some(expected_execpolicy_amendment.clone())
    );

    test.codex
        .submit(Op::ExecApproval {
            id: approval.effective_approval_id(),
            turn_id: None,
            decision: ReviewDecision::ApprovedExecpolicyAmendment {
                proposed_execpolicy_amendment: expected_execpolicy_amendment.clone(),
            },
        })
        .await?;
    wait_for_completion(&test).await;

    let policy_path = test.home.path().join("rules").join("default.rules");
    let policy_contents = fs::read_to_string(&policy_path)?;
    assert!(
        policy_contents
            .contains(r#"prefix_rule(pattern=["touch", "allow-prefix.txt"], decision="allow")"#),
        "unexpected policy contents: {policy_contents}"
    );

    let first_output = parse_result(
        &first_results
            .single_request()
            .function_call_output(call_id_first),
    );
    assert_eq!(first_output.exit_code.unwrap_or(0), 0);
    assert!(
        first_output.stdout.is_empty(),
        "unexpected stdout: {}",
        first_output.stdout
    );
    assert_eq!(
        fs::read_to_string(&allow_prefix_path)?,
        "",
        "unexpected file contents after first run"
    );

    let call_id_second = "allow-prefix-second";
    let (second_event, second_command) = ActionKind::RunCommand {
        command: "touch allow-prefix.txt",
    }
    .prepare(
        &test,
        &server,
        call_id_second,
        SandboxPermissions::UseDefault,
    )
    .await?;
    assert_eq!(second_command.as_deref(), Some(expected_command.as_str()));

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-allow-prefix-3"),
            second_event,
            ev_completed("resp-allow-prefix-3"),
        ]),
    )
    .await;
    let second_results = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-allow-prefix-2", "done"),
            ev_completed("resp-allow-prefix-4"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        "allow-prefix-second",
        approval_policy,
        sandbox_policy.clone(),
    )
    .await?;

    wait_for_completion_without_approval(&test).await;

    let second_output = parse_result(
        &second_results
            .single_request()
            .function_call_output(call_id_second),
    );
    assert_eq!(second_output.exit_code.unwrap_or(0), 0);
    assert!(
        second_output.stdout.is_empty(),
        "unexpected stdout: {}",
        second_output.stdout
    );
    assert_eq!(
        fs::read_to_string(&allow_prefix_path)?,
        "",
        "unexpected file contents after second run"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawned_subagent_execpolicy_amendment_propagates_to_parent_session() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let approval_policy = AskForApproval::UnlessTrusted;
    let sandbox_policy = SandboxPolicy::new_workspace_write_policy();
    let sandbox_policy_for_config = sandbox_policy.clone();
    let mut builder = test_codex().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(approval_policy);
        config.workspace_roots_explicit = false;
        let permission_profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
            &sandbox_policy_for_config,
            config.cwd.as_path(),
        );
        config
            .permissions
            .can_set_permission_profile(&permission_profile)
            .expect("set permission profile");
        config
            .permissions
            .set_workspace_roots(vec![config.cwd.clone()]);
        config
            .permissions
            .set_permission_profile(permission_profile)
            .expect("set permission profile");
        config.workspace_roots = config.permissions.workspace_roots().to_vec();
        config
            .features
            .enable(Feature::Collab)
            .expect("test config should allow feature update");
    });
    let test = builder.build(&server).await?;

    const PARENT_PROMPT: &str = "spawn a child that repeats a command";
    const CHILD_PROMPT: &str = "run the same command twice";
    const SPAWN_CALL_ID: &str = "spawn-child-1";
    const CHILD_CALL_ID_1: &str = "child-touch-1";
    const PARENT_CALL_ID_2: &str = "parent-touch-2";

    let child_file = test.cwd.path().join("subagent-allow-prefix.txt");
    let _ = fs::remove_file(&child_file);

    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
    }))?;
    mount_sse_once_match(
        &server,
        |req: &Request| body_contains(req, PARENT_PROMPT),
        sse(vec![
            ev_response_created("resp-parent-1"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                "multi_agent_v1",
                "spawn_agent",
                &spawn_args,
            ),
            ev_completed("resp-parent-1"),
        ]),
    )
    .await;

    let child_cmd_args = serde_json::to_string(&json!({
        "cmd": "touch subagent-allow-prefix.txt",
        "yield_time_ms": 10_000,
        "prefix_rule": ["touch", "subagent-allow-prefix.txt"],
    }))?;
    mount_sse_once_match(
        &server,
        |req: &Request| body_contains(req, CHILD_PROMPT) && !body_contains(req, SPAWN_CALL_ID),
        sse(vec![
            ev_response_created("resp-child-1"),
            ev_function_call(CHILD_CALL_ID_1, "exec_command", &child_cmd_args),
            ev_completed("resp-child-1"),
        ]),
    )
    .await;

    mount_sse_once_match(
        &server,
        |req: &Request| body_contains(req, CHILD_CALL_ID_1),
        sse(vec![
            ev_response_created("resp-child-2"),
            ev_assistant_message("msg-child-2", "child done"),
            ev_completed("resp-child-2"),
        ]),
    )
    .await;

    mount_sse_once_match(
        &server,
        |req: &Request| body_contains(req, SPAWN_CALL_ID),
        sse(vec![
            ev_response_created("resp-parent-2"),
            ev_assistant_message("msg-parent-2", "parent done"),
            ev_completed("resp-parent-2"),
        ]),
    )
    .await;

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-parent-3"),
            ev_function_call(PARENT_CALL_ID_2, "exec_command", &child_cmd_args),
            ev_completed("resp-parent-3"),
        ]),
    )
    .await;

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-parent-4"),
            ev_assistant_message("msg-parent-4", "parent rerun done"),
            ev_completed("resp-parent-4"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        PARENT_PROMPT,
        approval_policy,
        sandbox_policy.clone(),
    )
    .await?;

    let child = wait_for_spawned_thread(&test).await?;
    let approval_event = wait_for_event_with_timeout(
        &child,
        |event| {
            matches!(
                event,
                EventMsg::ExecApprovalRequest(_) | EventMsg::TurnComplete(_)
            )
        },
        Duration::from_secs(2),
    )
    .await;

    let EventMsg::ExecApprovalRequest(approval) = approval_event else {
        panic!("expected child approval before completion");
    };
    let expected_execpolicy_amendment = ExecPolicyAmendment::new(vec![
        "touch".to_string(),
        "subagent-allow-prefix.txt".to_string(),
    ]);
    assert_eq!(
        approval.proposed_execpolicy_amendment,
        Some(expected_execpolicy_amendment.clone())
    );

    child
        .submit(Op::ExecApproval {
            id: approval.effective_approval_id(),
            turn_id: None,
            decision: ReviewDecision::ApprovedExecpolicyAmendment {
                proposed_execpolicy_amendment: expected_execpolicy_amendment,
            },
        })
        .await?;

    let child_event = wait_for_event_with_timeout(
        &child,
        |event| {
            matches!(
                event,
                EventMsg::ExecApprovalRequest(_) | EventMsg::TurnComplete(_)
            )
        },
        Duration::from_secs(2),
    )
    .await;
    match child_event {
        EventMsg::TurnComplete(_) => {}
        EventMsg::ExecApprovalRequest(ev) => {
            panic!("unexpected second child approval request: {:?}", ev.command)
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(
        child_file.exists(),
        "expected subagent command to create file"
    );
    fs::remove_file(&child_file)?;
    assert!(
        !child_file.exists(),
        "expected child file to be removed before parent rerun"
    );

    submit_turn(
        &test,
        "parent reruns child command",
        approval_policy,
        sandbox_policy,
    )
    .await?;
    wait_for_completion_without_approval(&test).await;

    Ok(())
}

/// Verifies that an allowlisted script retains the originating named profile
/// when its shell tool call requests escalated, unsandboxed execution.
///
/// Tool owners use this pattern when a trusted wrapper must run outside the
/// current sandbox, but then needs to launch child commands back inside the
/// same sandbox with `codex sandbox -P`. The nested invocation must also pass
/// `--include-managed-config` so it continues to honor enterprise requirements.
/// The test proves both halves of that contract: the wrapper writes outside the
/// `:workspace` sandbox, while its inherited profile name remains `:workspace`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(unix)]
async fn allowed_escalated_exec_command_inherits_active_permission_profile() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    fs::write(
        home.path().join("config.toml"),
        WORKSPACE_PERMISSION_PROFILE_CONFIG,
    )?;

    let script_dir = tempfile::tempdir_in(std::env::current_dir()?)?;
    let script_path = script_dir.path().join("print-permission-profile.sh");
    let outside_path = script_dir.path().join("unsandboxed-marker");
    fs::write(
        &script_path,
        format!(
            r#"#!/bin/sh
# Print the inherited profile so the test can verify that it reached this script.
printenv CODEX_PERMISSION_PROFILE
touch {outside_path:?}
"#
        ),
    )?;

    let rules_dir = home.path().join("rules");
    fs::create_dir_all(&rules_dir)?;
    let script_pattern = serde_json::to_string(&script_path.to_string_lossy())?;
    fs::write(
        rules_dir.join("default.rules"),
        format!(r#"prefix_rule(pattern=["/bin/sh", {script_pattern}], decision="allow")"#),
    )?;

    let approval_policy = AskForApproval::OnRequest;
    let mut builder = test_codex().with_home(home).with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(approval_policy);
    });
    let test = builder.build(&server).await?;
    assert!(!outside_path.starts_with(test.config.cwd.as_path()));
    assert_active_workspace_permission_profile(&test);

    let call_id = "allowed-escalated-shell-inherits-permission-profile";
    let command = format!("/bin/sh {script_path:?}");
    let event = shell_event(
        call_id,
        &command,
        /*timeout_ms*/ 5_000,
        SandboxPermissions::RequireEscalated,
    )?;
    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-escalated-profile-1"),
            event,
            ev_completed("resp-escalated-profile-1"),
        ]),
    )
    .await;
    let results = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-escalated-profile-1", "done"),
            ev_completed("resp-escalated-profile-2"),
        ]),
    )
    .await;

    submit_turn_preserving_active_permission_profile(
        &test,
        "run the allowed script with escalated permissions",
        approval_policy,
    )
    .await?;

    wait_for_completion_without_approval(&test).await;

    let result = parse_result(&results.single_request().function_call_output(call_id));
    assert_eq!(
        result,
        CommandResult {
            exit_code: None,
            stdout: format!("{BUILT_IN_PERMISSION_PROFILE_WORKSPACE}\n"),
        },
        "the unsandboxed script should inherit CODEX_PERMISSION_PROFILE from the shell command"
    );
    assert!(
        outside_path.exists(),
        "allowed escalated script should run outside the :workspace sandbox"
    );

    Ok(())
}

#[cfg(unix)]
#[test_case(Some(ReviewDecision::Approved); "approved")]
#[test_case(Some(ReviewDecision::denied("rejected by user")); "denied")]
#[test_case(None; "allow_rule_skips_approval_without_widening")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_escalation_with_denied_reads(decision: Option<ReviewDecision>) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    if decision.is_none() {
        let rules = home.path().join("rules");
        fs::create_dir(&rules)?;
        fs::write(
            rules.join("default.rules"),
            r#"prefix_rule(pattern=["/bin/sh", "escalation.sh"], decision="allow")"#,
        )?;
    }
    let outside_marker = home.path().join("escalated-marker");
    let quoted_marker = shlex::try_join([outside_marker.to_string_lossy().as_ref()])?;
    let approval_policy = AskForApproval::OnRequest;
    let mut builder = test_codex()
        .with_home(home)
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(approval_policy);
            config.features.enable(Feature::UnifiedExec).unwrap();
            let mut file_system = PermissionProfile::workspace_write_with(
                &[],
                NetworkSandboxPolicy::Restricted,
                true,
                true,
            )
            .file_system_sandbox_policy();
            file_system.entries.push(FileSystemSandboxEntry::new(
                config.cwd.join("secret.txt").into(),
                FileSystemAccessMode::Deny,
            ));
            config
                .permissions
                .set_permission_profile(PermissionProfile::from_runtime_permissions(
                    &file_system,
                    NetworkSandboxPolicy::Restricted,
                ))
                .unwrap();
        })
        .with_workspace_setup(move |cwd, _fs| async move {
            fs::write(cwd.join("secret.txt"), "secret")?;
            fs::write(
                cwd.join("escalation.sh"),
                format!(
                    "printf ran > ran || exit 10; (printf outside > {quoted_marker}) 2> errors; \
                 if cat secret.txt > errors 2>&1; then exit 11; fi; \
                 grep -q secret.txt errors || exit 12; printf 'protected\\n'"
                ),
            )?;
            Ok(())
        });
    let test = builder.build(&server).await?;
    let workspace_marker = test.cwd.path().join("ran");
    let command = "/bin/sh escalation.sh";
    let call_id = "explicit-deny-read-escalation";
    let event = exec_command_event(
        call_id,
        command,
        Some(10_000),
        SandboxPermissions::RequireEscalated,
        /*justification*/ None,
    )?;
    mount_sse_once(&server, sse(vec![event, ev_completed("run")])).await;
    let response = mount_sse_once(&server, sse(vec![ev_completed("done")])).await;
    submit_turn_preserving_active_permission_profile(&test, "run the command", approval_policy)
        .await?;
    let approved = decision == Some(ReviewDecision::Approved);
    let rejected = decision.is_some() && !approved;
    if let Some(decision) = decision {
        let request = expect_exec_approval(&test, command).await;
        assert!(!outside_marker.exists() && !workspace_marker.exists());
        test.codex
            .submit(Op::ExecApproval {
                id: request.effective_approval_id(),
                turn_id: None,
                decision,
            })
            .await?;
    }
    wait_for_completion_without_approval(&test).await;
    let result = parse_result(&response.single_request().function_call_output(call_id));
    if rejected {
        assert!(result.stdout.contains("rejected by user"), "{result:?}");
        assert!(!workspace_marker.exists());
    } else {
        assert_eq!(result.stdout, "protected\n");
        assert_eq!(fs::read_to_string(workspace_marker)?, "ran");
    }
    assert_eq!(outside_marker.exists(), approved);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn invalid_requested_prefix_rule_falls_back_for_compound_command() -> Result<()> {
    let server = start_mock_server().await;
    let approval_policy = AskForApproval::OnRequest;
    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let sandbox_policy_for_config = sandbox_policy.clone();
    let mut builder = test_codex().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(approval_policy);
        config.workspace_roots_explicit = false;
        let permission_profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
            &sandbox_policy_for_config,
            config.cwd.as_path(),
        );
        config
            .permissions
            .can_set_permission_profile(&permission_profile)
            .expect("set permission profile");
        config
            .permissions
            .set_workspace_roots(vec![config.cwd.clone()]);
        config
            .permissions
            .set_permission_profile(permission_profile)
            .expect("set permission profile");
        config.workspace_roots = config.permissions.workspace_roots().to_vec();
    });
    let test = builder.build(&server).await?;

    let call_id = "invalid-prefix-rule";
    let command =
        "touch /tmp/codex-fallback-rule-test.txt && echo hello > /tmp/codex-fallback-rule-test.txt";
    let event = shell_event_with_prefix_rule(
        call_id,
        command,
        /*timeout_ms*/ 1_000,
        SandboxPermissions::RequireEscalated,
        Some(vec!["touch".to_string()]),
    )?;

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-invalid-prefix-1"),
            event,
            ev_completed("resp-invalid-prefix-1"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        "invalid-prefix-rule",
        approval_policy,
        sandbox_policy.clone(),
    )
    .await?;

    let approval = expect_exec_approval(&test, command).await;
    let amendment = approval
        .proposed_execpolicy_amendment
        .expect("should have a proposed execpolicy amendment");
    assert!(amendment.command.contains(&command.to_string()));

    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn approving_fallback_rule_for_compound_command_works() -> Result<()> {
    let server = start_mock_server().await;
    let approval_policy = AskForApproval::OnRequest;
    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let sandbox_policy_for_config = sandbox_policy.clone();
    let mut builder = test_codex().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(approval_policy);
        config.workspace_roots_explicit = false;
        let permission_profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
            &sandbox_policy_for_config,
            config.cwd.as_path(),
        );
        config
            .permissions
            .can_set_permission_profile(&permission_profile)
            .expect("set permission profile");
        config
            .permissions
            .set_workspace_roots(vec![config.cwd.clone()]);
        config
            .permissions
            .set_permission_profile(permission_profile)
            .expect("set permission profile");
        config.workspace_roots = config.permissions.workspace_roots().to_vec();
    });
    let test = builder.build(&server).await?;

    let call_id = "invalid-prefix-rule";
    let command =
        "touch /tmp/codex-fallback-rule-test.txt && echo hello > /tmp/codex-fallback-rule-test.txt";
    let event = shell_event_with_prefix_rule(
        call_id,
        command,
        /*timeout_ms*/ 10_000,
        SandboxPermissions::RequireEscalated,
        Some(vec!["touch".to_string()]),
    )?;

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-invalid-prefix-1"),
            event,
            ev_completed("resp-invalid-prefix-1"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        "invalid-prefix-rule",
        approval_policy,
        sandbox_policy.clone(),
    )
    .await?;

    let approval = expect_exec_approval(&test, command).await;
    let approval_id = approval.effective_approval_id();
    let amendment = approval
        .proposed_execpolicy_amendment
        .expect("should have a proposed execpolicy amendment");
    assert!(amendment.command.contains(&command.to_string()));

    test.codex
        .submit(Op::ExecApproval {
            id: approval_id,
            turn_id: None,
            decision: ReviewDecision::ApprovedExecpolicyAmendment {
                proposed_execpolicy_amendment: amendment.clone(),
            },
        })
        .await?;
    wait_for_completion(&test).await;

    let call_id = "invalid-prefix-rule-again";
    let command =
        "touch /tmp/codex-fallback-rule-test.txt && echo hello > /tmp/codex-fallback-rule-test.txt";
    let event = shell_event_with_prefix_rule(
        call_id,
        command,
        /*timeout_ms*/ 10_000,
        SandboxPermissions::RequireEscalated,
        Some(vec!["touch".to_string()]),
    )?;

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-invalid-prefix-1"),
            event,
            ev_completed("resp-invalid-prefix-1"),
        ]),
    )
    .await;
    let second_results = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-invalid-prefix-1", "done"),
            ev_completed("resp-invalid-prefix-2"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        "invalid-prefix-rule",
        approval_policy,
        sandbox_policy.clone(),
    )
    .await?;

    wait_for_completion_without_approval(&test).await;

    let second_output = parse_result(
        &second_results
            .single_request()
            .function_call_output(call_id),
    );
    assert_eq!(second_output.exit_code.unwrap_or(0), 0);
    assert!(
        second_output.stdout.is_empty(),
        "unexpected stdout: {}",
        second_output.stdout
    );

    Ok(())
}

// todo(dylan) add ScenarioSpec support for rules
#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn compound_command_with_one_safe_command_still_requires_approval() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let approval_policy = AskForApproval::UnlessTrusted;
    let sandbox_policy = SandboxPolicy::new_workspace_write_policy();
    let sandbox_policy_for_config = sandbox_policy.clone();
    let mut builder = test_codex().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(approval_policy);
        config.workspace_roots_explicit = false;
        let permission_profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
            &sandbox_policy_for_config,
            config.cwd.as_path(),
        );
        config
            .permissions
            .can_set_permission_profile(&permission_profile)
            .expect("set permission profile");
        config
            .permissions
            .set_workspace_roots(vec![config.cwd.clone()]);
        config
            .permissions
            .set_permission_profile(permission_profile)
            .expect("set permission profile");
        config.workspace_roots = config.permissions.workspace_roots().to_vec();
    });
    let test = builder.build(&server).await?;

    let rules_dir = test.home.path().join("rules");
    fs::create_dir_all(&rules_dir)?;
    fs::write(
        rules_dir.join("default.rules"),
        r#"prefix_rule(pattern=["touch", "allow-prefix.txt"], decision="allow")"#,
    )?;

    let call_id = "heredoc-with-chained-prefix";
    let command = "touch ./test.txt && rm ./test.txt";
    let (event, expected_command) = ActionKind::RunCommand { command }
        .prepare(&test, &server, call_id, SandboxPermissions::UseDefault)
        .await?;
    let expected_command =
        expected_command.expect("compound command should produce a shell command");

    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-heredoc-prefix-1"),
            event,
            ev_completed("resp-heredoc-prefix-1"),
        ]),
    )
    .await;
    let _ = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-heredoc-prefix-1", "done"),
            ev_completed("resp-heredoc-prefix-2"),
        ]),
    )
    .await;

    submit_turn(
        &test,
        "compound command",
        approval_policy,
        sandbox_policy.clone(),
    )
    .await?;

    let approval = expect_exec_approval(&test, expected_command.as_str()).await;
    test.codex
        .submit(Op::ExecApproval {
            id: approval.effective_approval_id(),
            turn_id: None,
            decision: ReviewDecision::denied("rejected by user"),
        })
        .await?;
    wait_for_completion(&test).await;

    Ok(())
}
