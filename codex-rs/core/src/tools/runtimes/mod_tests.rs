use super::*;
use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use crate::sandboxing::ExecOptions;
use crate::shell::ShellType;
use crate::tools::sandboxing::SandboxAttempt;
use crate::tools::sandboxing::SandboxOverride;
#[cfg(target_os = "macos")]
use codex_network_proxy::CODEX_PROXY_GIT_SSH_COMMAND_MARKER;
use codex_network_proxy::PROXY_ACTIVE_ENV_KEY;
use codex_network_proxy::PROXY_ENV_KEYS;
#[cfg(target_os = "macos")]
use codex_network_proxy::PROXY_GIT_SSH_COMMAND_ENV_KEY;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxSelection;
use codex_sandboxing::SandboxType;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use core_test_support::PathBufExt;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::process::Command;
use tempfile::tempdir;

fn shell_with_snapshot(
    shell_type: ShellType,
    shell_path: &str,
    snapshot_path: AbsolutePathBuf,
) -> (Shell, AbsolutePathBuf) {
    (
        Shell {
            shell_type,
            shell_path: PathBuf::from(shell_path),
        },
        snapshot_path,
    )
}

#[tokio::test]
async fn explicit_escalation_prepares_exec_without_managed_network() -> anyhow::Result<()> {
    let dir = tempdir().expect("create temp dir");
    let command_cwd = dir.path().join("command").abs();
    let native_sandbox_policy_cwd = dir.path().join("sandbox-policy").abs();
    let mut env = HashMap::from([
        ("CUSTOM_ENV".to_string(), "kept".to_string()),
        (PROXY_ACTIVE_ENV_KEY.to_string(), "1".to_string()),
    ]);
    for key in PROXY_ENV_KEYS {
        env.insert((*key).to_string(), "http://127.0.0.1:43123".to_string());
    }
    #[cfg(target_os = "macos")]
    env.insert(
        PROXY_GIT_SSH_COMMAND_ENV_KEY.to_string(),
        format!("{CODEX_PROXY_GIT_SSH_COMMAND_MARKER}ssh"),
    );

    let command = codex_sandboxing::SandboxCommand {
        program: "/bin/echo".into(),
        args: vec!["ok".to_string()],
        cwd: PathUri::from_abs_path(&command_cwd),
        env: exec_env_for_sandbox_permissions(&env, SandboxPermissions::RequireEscalated),
        managed_network: None,
        additional_permissions: None,
    };
    assert_eq!(command.cwd, PathUri::from_abs_path(&command_cwd));
    let sandbox_policy_cwd = PathUri::from_abs_path(&native_sandbox_policy_cwd);
    let options = ExecOptions {
        expiration: ExecExpiration::DefaultTimeout,
        capture_policy: ExecCapturePolicy::ShellTool,
    };
    let permissions = PermissionProfile::Disabled;
    let manager = SandboxManager::new();
    let attempt = SandboxAttempt {
        sandbox_override: SandboxOverride::NoOverride,
        local_selection: SandboxSelection::Explicit(SandboxType::None),
        sandbox_requested: false,
        permissions: &permissions,
        exec_server_permissions: &permissions,
        enforce_managed_network: false,
        manager: &manager,
        sandbox_cwd: &sandbox_policy_cwd,
        workspace_roots: std::slice::from_ref(&sandbox_policy_cwd),
        sandbox_exe: None,
        use_legacy_landlock: false,
        windows_sandbox_type: SandboxType::None,
        windows_sandbox_level: WindowsSandboxLevel::Disabled,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };

    let exec_request = attempt
        .env_for(
            command, options, /*network*/ None, /*environment_id*/ None,
        )
        .await
        .expect("prepare exec request");

    assert_eq!(*exec_request.cwd(), PathUri::from_abs_path(&command_cwd));
    assert_eq!(
        *exec_request.sandbox_policy_cwd(),
        PathUri::from_abs_path(&native_sandbox_policy_cwd)
    );
    assert!(!exec_request.has_network_proxy());
    for key in PROXY_ENV_KEYS {
        assert_eq!(exec_request.env().get(*key), None, "{key} should be unset");
    }
    #[cfg(target_os = "macos")]
    assert_eq!(exec_request.env().get(PROXY_GIT_SSH_COMMAND_ENV_KEY), None);
    assert_eq!(
        exec_request.env().get("CUSTOM_ENV"),
        Some(&"kept".to_string())
    );

    Ok(())
}

#[test]
fn explicit_escalation_preserves_user_ca_env() {
    let env = HashMap::from([
        (PROXY_ACTIVE_ENV_KEY.to_string(), "1".to_string()),
        (
            "SSL_CERT_FILE".to_string(),
            "/tmp/custom-ca.pem".to_string(),
        ),
    ]);

    let env = exec_env_for_sandbox_permissions(&env, SandboxPermissions::RequireEscalated);

    assert_eq!(
        env.get("SSL_CERT_FILE"),
        Some(&"/tmp/custom-ca.pem".to_string())
    );
}

#[cfg(unix)]
#[test]
fn runtime_path_prepends_records_runtime_path_prepend() {
    let mut env = HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
    let mut runtime_path_prepends = RuntimePathPrepends::default();

    runtime_path_prepends.prepend(&mut env, PathBuf::from("/package/codex-path").as_path());

    assert_eq!(
        env.get("PATH").map(String::as_str),
        Some("/package/codex-path:/usr/bin:/bin"),
        "runtime PATH prepend should update the live exec environment"
    );
    assert_eq!(
        runtime_path_prepends.entries,
        vec!["/package/codex-path"],
        "runtime PATH prepend should be recorded for snapshot replay"
    );
}

#[cfg(unix)]
#[test]
fn runtime_path_prepends_drops_empty_path_entries() {
    let mut env = HashMap::from([(
        "PATH".to_string(),
        ":/usr/bin:/package/codex-path::/bin:".to_string(),
    )]);
    let mut runtime_path_prepends = RuntimePathPrepends::default();

    runtime_path_prepends.prepend(&mut env, PathBuf::from("/package/codex-path").as_path());

    assert_eq!(
        env.get("PATH").map(String::as_str),
        Some("/package/codex-path:/usr/bin:/bin"),
        "empty PATH entries should be dropped instead of preserving current-directory lookup"
    );
    assert_eq!(
        runtime_path_prepends.entries,
        vec!["/package/codex-path"],
        "deduped runtime PATH prepend should still be recorded once"
    );
}

#[cfg(unix)]
#[test]
fn runtime_path_prepends_ignores_empty_path_entry() {
    let mut env = HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
    let mut runtime_path_prepends = RuntimePathPrepends::default();

    runtime_path_prepends.prepend(&mut env, PathBuf::new().as_path());

    assert_eq!(
        env.get("PATH").map(String::as_str),
        Some("/usr/bin:/bin"),
        "empty runtime PATH prepend should leave PATH unchanged"
    );
    assert_eq!(
        runtime_path_prepends,
        RuntimePathPrepends::default(),
        "empty runtime PATH prepend should not be recorded for snapshot replay"
    );
}

#[test]
fn explicit_escalation_keeps_user_proxy_env_without_codex_marker() {
    let env = HashMap::from([
        (
            "HTTP_PROXY".to_string(),
            "http://user.proxy:8080".to_string(),
        ),
        ("CUSTOM_ENV".to_string(), "kept".to_string()),
    ]);

    let env = exec_env_for_sandbox_permissions(&env, SandboxPermissions::RequireEscalated);

    assert_eq!(
        env.get("HTTP_PROXY"),
        Some(&"http://user.proxy:8080".to_string())
    );
    assert_eq!(env.get("CUSTOM_ENV"), Some(&"kept".to_string()));
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_bootstraps_in_user_shell() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(&snapshot_path, "# Snapshot file\n").expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Zsh, "/bin/zsh", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "echo hello".to_string(),
    ];

    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );

    assert_eq!(rewritten[0], "/bin/zsh");
    assert_eq!(rewritten[1], "-c");
    assert!(rewritten[2].contains("if . '"));
    assert!(rewritten[2].contains("exec '/bin/bash' -c 'echo hello'"));
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_escapes_single_quotes() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(&snapshot_path, "# Snapshot file\n").expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Zsh, "/bin/zsh", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "echo 'hello'".to_string(),
    ];

    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );

    assert!(rewritten[2].contains(r#"exec '/bin/bash' -c 'echo '"'"'hello'"'"''"#));
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_uses_bash_bootstrap_shell() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(&snapshot_path, "# Snapshot file\n").expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/zsh".to_string(),
        "-lc".to_string(),
        "echo hello".to_string(),
    ];

    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );

    assert_eq!(rewritten[0], "/bin/bash");
    assert_eq!(rewritten[1], "-c");
    assert!(rewritten[2].contains("if . '"));
    assert!(rewritten[2].contains("exec '/bin/zsh' -c 'echo hello'"));
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_uses_sh_bootstrap_shell() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(&snapshot_path, "# Snapshot file\n").expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Sh, "/bin/sh", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "echo hello".to_string(),
    ];

    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );

    assert_eq!(rewritten[0], "/bin/sh");
    assert_eq!(rewritten[1], "-c");
    assert!(rewritten[2].contains("if . '"));
    assert!(rewritten[2].contains("exec '/bin/bash' -c 'echo hello'"));
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_preserves_trailing_args() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(&snapshot_path, "# Snapshot file\n").expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Zsh, "/bin/zsh", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s %s' \"$0\" \"$1\"".to_string(),
        "arg0".to_string(),
        "arg1".to_string(),
    ];

    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );

    assert!(
        rewritten[2]
            .contains(r#"exec '/bin/bash' -c 'printf '"'"'%s %s'"'"' "$0" "$1"' 'arg0' 'arg1'"#)
    );
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_reuses_brokered_session_zsh() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(&snapshot_path, "# Snapshot file\n").expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Zsh, "/bin/zsh", snapshot_path.abs());
    let copy_key = format!("{SNAPSHOT_BROKERED_VALUE_ENV_PREFIX}OPENAI_API_KEY");
    let env = HashMap::from([
        (
            CREDENTIAL_BROKER_ACTIVE_ENV_KEY.to_string(),
            "1".to_string(),
        ),
        (copy_key.clone(), "dummy-openai-key".to_string()),
    ]);
    for (flag, expected_flag) in [("-c", "-fc"), ("-lc", "-lfc")] {
        let command = vec![
            "/bin/zsh".to_string(),
            flag.to_string(),
            "printf '%s %s' \"$0\" \"$1\"".to_string(),
            "arg0".to_string(),
            "arg1".to_string(),
        ];

        let rewritten = maybe_wrap_shell_lc_with_snapshot(
            &command,
            &session_shell,
            Some(&shell_snapshot),
            &HashMap::new(),
            &env,
            &RuntimePathPrepends::default(),
        );

        assert_eq!(rewritten[0], "/bin/zsh");
        assert_eq!(rewritten[1], expected_flag);
        assert!(rewritten[2].contains(&format!(
            "if . '{}' >/dev/null 2>&1; then :; fi",
            shell_single_quote(&shell_snapshot.to_string_lossy())
        )));
        assert!(rewritten[2].ends_with(&command[2]));
        assert!(rewritten[2].contains(&format!("unset {copy_key} || exit 1")));
        assert!(!rewritten[2].contains("exec '/bin/zsh'"));
        assert_eq!(rewritten[3..], command[3..]);
    }
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_restores_explicit_override_precedence() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport TEST_ENV_SNAPSHOT=global\nexport SNAPSHOT_ONLY=from_snapshot\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s|%s' \"$TEST_ENV_SNAPSHOT\" \"${SNAPSHOT_ONLY-unset}\"".to_string(),
    ];
    let explicit_env_overrides =
        HashMap::from([("TEST_ENV_SNAPSHOT".to_string(), "worktree".to_string())]);
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &explicit_env_overrides,
        &HashMap::from([("TEST_ENV_SNAPSHOT".to_string(), "worktree".to_string())]),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env("TEST_ENV_SNAPSHOT", "worktree")
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "worktree|from_snapshot"
    );
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_restores_codex_metadata_from_env() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport CODEX_THREAD_ID='parent-thread'\nexport CODEX_TOOL_CALL_ID='parent-call'\nexport CODEX_VERSION='old-version'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s|%s|%s' \"$CODEX_THREAD_ID\" \"$CODEX_TOOL_CALL_ID\" \"$CODEX_VERSION\""
            .to_string(),
    ];
    let env = HashMap::from([
        ("CODEX_THREAD_ID".to_string(), "nested-thread".to_string()),
        ("CODEX_TOOL_CALL_ID".to_string(), "nested-call".to_string()),
        (
            "CODEX_VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        ),
    ]);
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &env,
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .envs(env)
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!("nested-thread|nested-call|", env!("CARGO_PKG_VERSION")),
    );
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_restores_permission_profile_from_env() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport CODEX_PERMISSION_PROFILE='parent-profile'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printenv CODEX_PERMISSION_PROFILE".to_string(),
    ];
    let env = HashMap::from([(
        CODEX_PERMISSION_PROFILE_ENV_VAR.to_string(),
        "current-profile".to_string(),
    )]);
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &env,
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env(CODEX_PERMISSION_PROFILE_ENV_VAR, "current-profile")
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "current-profile\n");
}

#[test_case::test_case(CODEX_PERMISSION_PROFILE_ENV_VAR; "permission profile")]
#[test_case::test_case(CODEX_TOOL_CALL_ID_ENV_VAR; "tool call ID")]
fn maybe_wrap_shell_lc_with_snapshot_unsets_absent_metadata(key: &str) -> anyhow::Result<()> {
    let dir = tempdir()?;
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        format!("# Snapshot file\nexport {key}='stale-value'\n"),
    )?;
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        format!("printenv {key}"),
    ];
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env_remove(key)
        .output()?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    Ok(())
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_restores_apply_patch_rollout_state() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS='stale'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printenv CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS".to_string(),
    ];
    let env = HashMap::from([(
        CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS_ENV_VAR.to_string(),
        "1".to_string(),
    )]);
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &env,
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env(CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS_ENV_VAR, "1")
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "1\n");

    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env_remove(CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS_ENV_VAR)
        .output()
        .expect("run rewritten command");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_restores_reserved_metrics_output_env() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport CODEX_PLUGIN_METRICS_OUTPUT='/stale/path'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s' \"${CODEX_PLUGIN_METRICS_OUTPUT-unset}\"".to_string(),
    ];

    for (live_value, expected) in [(None, "unset"), (Some("/private/path"), "/private/path")] {
        let env = live_value
            .map(|value| {
                HashMap::from([(PLUGIN_METRICS_OUTPUT_ENV_VAR.to_string(), value.to_string())])
            })
            .unwrap_or_default();
        let rewritten = maybe_wrap_shell_lc_with_snapshot(
            &command,
            &session_shell,
            Some(&shell_snapshot),
            &HashMap::new(),
            &env,
            &RuntimePathPrepends::default(),
        );
        let mut process = Command::new(&rewritten[0]);
        process.args(&rewritten[1..]);
        match live_value {
            Some(value) => process.env(PLUGIN_METRICS_OUTPUT_ENV_VAR, value),
            None => process.env_remove(PLUGIN_METRICS_OUTPUT_ENV_VAR),
        };
        let output = process.output().expect("run rewritten command");

        assert!(output.status.success(), "command failed: {output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    }
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_restores_proxy_env_from_process_env() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\n\
         export PIP_PROXY='http://127.0.0.1:8080'\n\
         export HTTP_PROXY='http://127.0.0.1:8080'\n\
         export http_proxy='http://127.0.0.1:8080'\n\
         export GIT_SSH_COMMAND='ssh -o ProxyCommand=stale'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s\\n%s\\n%s\\n%s' \"$PIP_PROXY\" \"$HTTP_PROXY\" \"$http_proxy\" \"$GIT_SSH_COMMAND\""
            .to_string(),
    ];
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env(PROXY_ACTIVE_ENV_KEY, "1")
        .env("PIP_PROXY", "http://127.0.0.1:4321")
        .env("HTTP_PROXY", "http://127.0.0.1:4321")
        .env("http_proxy", "http://127.0.0.1:4321")
        .env("GIT_SSH_COMMAND", "ssh -o ProxyCommand=fresh")
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "http://127.0.0.1:4321\n\
         http://127.0.0.1:4321\n\
         http://127.0.0.1:4321\n\
         ssh -o ProxyCommand=stale"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn maybe_wrap_shell_lc_with_snapshot_refreshes_codex_proxy_git_ssh_command() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    let stale_command = format!(
        "{CODEX_PROXY_GIT_SSH_COMMAND_MARKER}ssh -o ProxyCommand='nc -X 5 -x 127.0.0.1:8081 %h %p'"
    );
    let fresh_command = format!(
        "{CODEX_PROXY_GIT_SSH_COMMAND_MARKER}ssh -o ProxyCommand='nc -X 5 -x 127.0.0.1:48081 %h %p'"
    );
    std::fs::write(
        &snapshot_path,
        format!(
            "# Snapshot file\nexport {PROXY_GIT_SSH_COMMAND_ENV_KEY}='{}'\n",
            shell_single_quote(&stale_command)
        ),
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        format!("printf '%s' \"${PROXY_GIT_SSH_COMMAND_ENV_KEY}\""),
    ];
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env(PROXY_GIT_SSH_COMMAND_ENV_KEY, &fresh_command)
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), fresh_command);
}

#[cfg(target_os = "macos")]
#[test]
fn maybe_wrap_shell_lc_with_snapshot_restores_custom_git_ssh_command() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    let stale_command = format!(
        "{CODEX_PROXY_GIT_SSH_COMMAND_MARKER}ssh -o ProxyCommand='nc -X 5 -x 127.0.0.1:8081 %h %p'"
    );
    let custom_command = "ssh -o ProxyCommand='tsh proxy ssh --cluster=dev %r@%h:%p'";
    std::fs::write(
        &snapshot_path,
        format!(
            "# Snapshot file\nexport {PROXY_GIT_SSH_COMMAND_ENV_KEY}='{}'\n",
            shell_single_quote(&stale_command)
        ),
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        format!("printf '%s' \"${PROXY_GIT_SSH_COMMAND_ENV_KEY}\""),
    ];
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env(PROXY_GIT_SSH_COMMAND_ENV_KEY, custom_command)
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), custom_command);
}

#[cfg(target_os = "macos")]
#[test]
fn maybe_wrap_shell_lc_with_snapshot_clears_stale_codex_git_ssh_command_without_live_command() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    let stale_command = format!(
        "{CODEX_PROXY_GIT_SSH_COMMAND_MARKER}ssh -o ProxyCommand='nc -X 5 -x 127.0.0.1:8081 %h %p'"
    );
    std::fs::write(
        &snapshot_path,
        format!(
            "# Snapshot file\nexport {PROXY_GIT_SSH_COMMAND_ENV_KEY}='{}'\n",
            shell_single_quote(&stale_command)
        ),
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        format!(
            "if [ \"${{{PROXY_GIT_SSH_COMMAND_ENV_KEY}+x}}\" = x ]; then printf 'set'; else printf 'unset'; fi"
        ),
    ];
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env_remove(PROXY_GIT_SSH_COMMAND_ENV_KEY)
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "unset");
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_keeps_user_proxy_env_when_proxy_inactive() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport HTTP_PROXY='http://user.proxy:8080'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s' \"$HTTP_PROXY\"".to_string(),
    ];
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );
    let mut command = Command::new(&rewritten[0]);
    command.args(&rewritten[1..]);
    for key in PROXY_ENV_KEYS {
        command.env_remove(key);
    }
    let output = command.output().expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "http://user.proxy:8080"
    );
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_restores_live_env_when_snapshot_proxy_active() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        format!(
            "# Snapshot file\n\
             export {PROXY_ACTIVE_ENV_KEY}='1'\n\
             export PIP_PROXY='http://127.0.0.1:8080'\n\
             export HTTP_PROXY='http://127.0.0.1:8080'\n"
        ),
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        format!(
            "if [ \"${{PIP_PROXY+x}}\" = x ]; then printf 'pip:%s\\n' \"$PIP_PROXY\"; else printf 'pip:unset\\n'; fi; \
             printf 'http:%s\\n' \"$HTTP_PROXY\"; \
             if [ \"${{{PROXY_ACTIVE_ENV_KEY}+x}}\" = x ]; then printf 'active:%s' \"${PROXY_ACTIVE_ENV_KEY}\"; else printf 'active:unset'; fi"
        ),
    ];
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::from([(
            "HTTP_PROXY".to_string(),
            "http://user.proxy:8080".to_string(),
        )]),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env("HTTP_PROXY", "http://user.proxy:8080")
        .env_remove("PIP_PROXY")
        .env_remove(PROXY_ACTIVE_ENV_KEY)
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "pip:unset\nhttp:http://user.proxy:8080\nactive:unset"
    );
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_keeps_snapshot_path_without_override() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport PATH='/snapshot/bin'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s' \"$PATH\"".to_string(),
    ];
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &HashMap::new(),
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "/snapshot/bin");
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_applies_explicit_path_override() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport PATH='/snapshot/bin'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s' \"$PATH\"".to_string(),
    ];
    let explicit_env_overrides = HashMap::from([("PATH".to_string(), "/worktree/bin".to_string())]);
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &explicit_env_overrides,
        &HashMap::from([("PATH".to_string(), "/worktree/bin".to_string())]),
        &RuntimePathPrepends::default(),
    );
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env("PATH", "/worktree/bin")
        .output()
        .expect("run rewritten command");

    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "/worktree/bin");
}

#[cfg(unix)]
#[test]
fn maybe_wrap_shell_lc_with_snapshot_preserves_package_path_prepend() -> anyhow::Result<()> {
    let (stdout, package_path_dir) =
        run_snapshot_path_probe_with_runtime_path_prepend(HashMap::new())?;

    assert_eq!(
        stdout,
        format!("{}:/snapshot/bin", package_path_dir.display()),
        "package path prepend should replay ahead of snapshot PATH"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn maybe_wrap_shell_lc_with_snapshot_applies_runtime_path_prepend_after_explicit_path_override()
-> anyhow::Result<()> {
    let (stdout, package_path_dir) = run_snapshot_path_probe_with_runtime_path_prepend(
        HashMap::from([("PATH".to_string(), "/worktree/bin".to_string())]),
    )?;

    assert_eq!(
        stdout,
        format!("{}:/worktree/bin", package_path_dir.display()),
        "explicit PATH override should suppress snapshot PATH while preserving runtime prepend"
    );
    Ok(())
}

#[cfg(unix)]
fn run_snapshot_path_probe_with_runtime_path_prepend(
    explicit_env_overrides: HashMap<String, String>,
) -> anyhow::Result<(String, PathBuf)> {
    let dir = tempdir()?;
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport PATH='/snapshot/bin'\n",
    )?;
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s' \"$PATH\"".to_string(),
    ];
    let package_path_dir = dir.path().join("codex-path");
    let mut env = HashMap::from([("PATH".to_string(), "/worktree/bin".to_string())]);
    let mut runtime_path_prepends = RuntimePathPrepends::default();
    runtime_path_prepends.prepend(&mut env, package_path_dir.as_path());
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &explicit_env_overrides,
        &env,
        &runtime_path_prepends,
    );
    let path = env
        .get("PATH")
        .ok_or_else(|| anyhow::anyhow!("PATH should be set"))?;
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env("PATH", path)
        .output()?;

    assert!(output.status.success(), "command failed: {output:?}");
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        package_path_dir,
    ))
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_does_not_embed_override_values_in_argv() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport OPENAI_API_KEY='snapshot-value'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
        "/bin/bash".to_string(),
        "-lc".to_string(),
        "printf '%s' \"$OPENAI_API_KEY\"".to_string(),
    ];
    let explicit_env_overrides = HashMap::from([
        (
            "OPENAI_API_KEY".to_string(),
            "super-secret-value".to_string(),
        ),
        (
            "openai_identity_token_file".to_string(),
            "/run/identity-token".to_string(),
        ),
    ]);
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &explicit_env_overrides,
        &HashMap::from([(
            "OPENAI_API_KEY".to_string(),
            "super-secret-value".to_string(),
        )]),
        &RuntimePathPrepends::default(),
    );

    assert!(!rewritten[2].contains("super-secret-value"));
    assert!(!rewritten[2].contains("openai_identity_token_file"));
    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env("OPENAI_API_KEY", "super-secret-value")
        .output()
        .expect("run rewritten command");
    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "super-secret-value"
    );
}

#[test]
fn maybe_wrap_shell_lc_with_snapshot_preserves_unset_override_variables() {
    let dir = tempdir().expect("create temp dir");
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(
        &snapshot_path,
        "# Snapshot file\nexport CODEX_TEST_UNSET_OVERRIDE='snapshot-value'\n",
    )
    .expect("write snapshot");
    let (session_shell, shell_snapshot) =
        shell_with_snapshot(ShellType::Bash, "/bin/bash", snapshot_path.abs());
    let command = vec![
            "/bin/bash".to_string(),
            "-lc".to_string(),
            "if [ \"${CODEX_TEST_UNSET_OVERRIDE+x}\" = x ]; then printf 'set:%s' \"$CODEX_TEST_UNSET_OVERRIDE\"; else printf 'unset'; fi".to_string(),
        ];
    let explicit_env_overrides = HashMap::from([(
        "CODEX_TEST_UNSET_OVERRIDE".to_string(),
        "worktree-value".to_string(),
    )]);
    let rewritten = maybe_wrap_shell_lc_with_snapshot(
        &command,
        &session_shell,
        Some(&shell_snapshot),
        &explicit_env_overrides,
        &HashMap::new(),
        &RuntimePathPrepends::default(),
    );

    let output = Command::new(&rewritten[0])
        .args(&rewritten[1..])
        .env_remove("CODEX_TEST_UNSET_OVERRIDE")
        .output()
        .expect("run rewritten command");
    assert!(output.status.success(), "command failed: {output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "unset");
}

#[test]
fn brokered_credential_exports_stop_when_startup_makes_credential_readonly() {
    let copy_key = format!("{SNAPSHOT_BROKERED_VALUE_ENV_PREFIX}OPENAI_API_KEY");
    let env = HashMap::from([(copy_key.clone(), "dummy-openai-key".to_string())]);
    let restore = build_brokered_credential_exports(&env, /*remove_copies*/ true);

    let unset_output = Command::new("/bin/bash")
        .args([
            "-c",
            &format!(
                "unset OPENAI_API_KEY\n{restore}\nprintf '%s|%s' \"${{OPENAI_API_KEY-unset}}\" \"${{{copy_key}-unset}}\""
            ),
        ])
        .env(&copy_key, "dummy-openai-key")
        .output()
        .expect("run unset credential probe");
    assert!(
        unset_output.status.success(),
        "unset credential probe failed: {unset_output:?}"
    );
    assert_eq!(String::from_utf8_lossy(&unset_output.stdout), "unset|unset");

    let empty_output = Command::new("/bin/bash")
        .args([
            "-c",
            &format!(
                "OPENAI_API_KEY=\n{restore}\nprintf '%s|%s' \"$OPENAI_API_KEY\" \"${{{copy_key}-unset}}\""
            ),
        ])
        .env(&copy_key, "dummy-openai-key")
        .output()
        .expect("run empty credential probe");
    assert!(
        empty_output.status.success(),
        "empty credential probe failed: {empty_output:?}"
    );
    assert_eq!(String::from_utf8_lossy(&empty_output.stdout), "|unset");

    let unchanged_output = Command::new("/bin/bash")
        .args([
            "-c",
            &format!(
                "readonly OPENAI_API_KEY=dummy-openai-key\n{restore}\nprintf '%s|%s' \"$OPENAI_API_KEY\" \"${{{copy_key}-unset}}\""
            ),
        ])
        .env(&copy_key, "dummy-openai-key")
        .output()
        .expect("run unchanged readonly credential probe");
    assert!(
        unchanged_output.status.success(),
        "unchanged readonly credential probe failed: {unchanged_output:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&unchanged_output.stdout),
        "dummy-openai-key|unset"
    );

    let xtrace_output = Command::new("/bin/bash")
        .args([
            "-c",
            &format!(
                "OPENAI_API_KEY=real-openai-key\nset -x\n{restore}\nset +x\nprintf '%s|%s' \"$OPENAI_API_KEY\" \"${{{copy_key}-unset}}\""
            ),
        ])
        .env(&copy_key, "dummy-openai-key")
        .output()
        .expect("run xtrace credential probe");
    assert!(
        xtrace_output.status.success(),
        "xtrace credential probe failed: {xtrace_output:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&xtrace_output.stdout),
        "dummy-openai-key|unset"
    );
    assert!(!String::from_utf8_lossy(&xtrace_output.stderr).contains("real-openai-key"));

    let unset_marker_key = format!("{SNAPSHOT_BROKERED_UNSET_ENV_PREFIX}OPENAI_API_KEY");
    let unset_env = HashMap::from([(unset_marker_key.clone(), "1".to_string())]);
    let unset_restore = build_brokered_credential_exports(&unset_env, /*remove_copies*/ true);
    let readonly_empty_output = Command::new("/bin/bash")
        .args([
            "-c",
            &format!(
                "readonly OPENAI_API_KEY=\n{unset_restore}\nprintf '%s|%s' \"$OPENAI_API_KEY\" \"${{{unset_marker_key}-unset}}\""
            ),
        ])
        .env(&unset_marker_key, "1")
        .output()
        .expect("run readonly empty credential probe");
    assert!(
        readonly_empty_output.status.success(),
        "readonly empty credential probe failed: {readonly_empty_output:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&readonly_empty_output.stdout),
        "|unset"
    );

    let output = Command::new("/bin/bash")
        .args([
            "-c",
            &format!("readonly OPENAI_API_KEY=real-openai-key\n{restore}\nprintf should-not-run"),
        ])
        .env(copy_key, "dummy-openai-key")
        .output()
        .expect("run readonly credential probe");

    assert!(!output.status.success(), "command unexpectedly succeeded");
    assert!(
        output.stdout.is_empty(),
        "protected command unexpectedly ran"
    );
}
