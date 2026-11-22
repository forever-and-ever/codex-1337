//! Exercises daemon startup and excluded resume/fork pickers with Cargo's CLI executable.

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use std::collections::VecDeque;
use std::fs;
use std::process::Command;
use std::time::Duration;

#[tokio::test]
#[cfg(unix)]
async fn auto_daemon_start_attaches_to_shared_server() -> Result<()> {
    daemon_startup("start").await
}

#[tokio::test]
#[cfg(unix)]
async fn agents_show_status_without_a_competing_daemon_restart() -> Result<()> {
    daemon_startup("agents").await
}

#[tokio::test]
#[cfg(unix)]
async fn agents_defers_launch_directory_trust_until_starting_a_task() -> Result<()> {
    daemon_startup("agents-untrusted").await
}

#[tokio::test]
#[cfg(unix)]
async fn discovered_daemon_reconnects_after_explicit_cli_restart_with_auto_start_disabled()
-> Result<()> {
    daemon_startup("discovered").await
}

#[tokio::test]
#[cfg(unix)]
async fn incompatible_daemon_can_cancel() -> Result<()> {
    daemon_startup("mismatch-cancel").await
}

#[tokio::test]
#[cfg(unix)]
async fn incompatible_daemon_can_run_embedded() -> Result<()> {
    daemon_startup("mismatch-embedded").await
}

#[tokio::test]
#[cfg(unix)]
async fn incompatible_daemon_can_restart_with_confirmed_settings() -> Result<()> {
    daemon_startup("mismatch-restart").await
}

#[tokio::test]
#[cfg(unix)]
async fn fresh_daemon_requires_confirmation_to_disable_shared_features() -> Result<()> {
    daemon_startup("mismatch-disable").await
}

#[tokio::test]
#[cfg(unix)]
async fn stopped_daemon_requires_confirmation_to_disable_persisted_features() -> Result<()> {
    daemon_startup("mismatch-disable-persisted").await
}

#[tokio::test]
async fn daemon_exclusion_survives_resume_picker() -> Result<()> {
    daemon_startup("resume").await
}

#[tokio::test]
async fn daemon_exclusion_survives_fork_picker() -> Result<()> {
    daemon_startup("fork").await
}

#[tokio::test]
#[cfg(windows)]
async fn elevated_local_tui_uses_embedded_without_starting_daemon() -> Result<()> {
    if !codex_app_server_daemon::is_elevated()? {
        eprintln!("requires an elevated Windows runner");
        return Ok(());
    }
    for command in ["elevated", "elevated-resume", "elevated-fork"] {
        daemon_startup(command).await?;
    }
    let codex = codex_utils_cargo_bin::cargo_bin("codex")?;
    let home = tempfile::tempdir()?;
    for operation in ["start", "restart"] {
        let output = Command::new(&codex)
            .env("CODEX_HOME", home.path())
            .args(["app-server", "daemon", operation])
            .output()?;
        ensure!(
            !output.status.success(),
            "{operation} must reject elevation"
        );
        ensure!(
            String::from_utf8_lossy(&output.stderr).contains("non-elevated terminal"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    ensure!(!home.path().join("app-server-daemon").exists());
    Ok(())
}

#[tokio::test]
#[cfg(windows)]
async fn restrictive_launcher_uses_embedded_if_daemon_cannot_start() -> Result<()> {
    const CHILD: &str = "CODEX_TEST_RESTRICTIVE_DAEMON_START";
    if std::env::var_os(CHILD).is_some() {
        // The PTY creates its own breakaway-permitted job. Add Cargo's
        // restriction *inside* the PTY so it is the CLI's innermost job.
        let job = codex_utils_pty::JobObject::create_without_breakaway()?;
        let codex = codex_utils_cargo_bin::cargo_bin("codex")?;
        let mut command = tokio::process::Command::new(codex);
        command
            .arg("--no-alt-screen")
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            // This interactive CLI must inherit the PTY console rather than use
            // the background helper's CREATE_NO_WINDOW launch policy.
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED)
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        ensure!(
            job.assign_and_resume_process(child.id().context("missing CLI pid")?)?
                .is_some(),
            "CLI job assignment failed"
        );
        let status = child.wait().await?;
        ensure!(status.success(), "CLI exited: {status}");
        return Ok(());
    }
    daemon_startup("restrictive-job").await
}

async fn daemon_startup(command: &str) -> Result<()> {
    #[cfg(windows)]
    let elevated = codex_app_server_daemon::is_elevated()?;
    #[cfg(not(windows))]
    let elevated = false;
    let elevated_launch = command.starts_with("elevated");
    let codex = codex_utils_cargo_bin::cargo_bin("codex")?.canonicalize()?;
    let workspace = tempfile::tempdir()?;
    let workspace_path = workspace.path().canonicalize()?;
    let home = tempfile::tempdir()?;
    // Keep cold Rosetta translation outside the timed startup assertions.
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    anyhow::ensure!(
        Command::new(&codex)
            .env("CODEX_HOME", home.path())
            .arg("--version")
            .output()?
            .status
            .success(),
        "failed to prepare CLI test executable"
    );
    fs::write(
        home.path().join("config.toml"),
        format!(
            "model = \"gpt-5.6-terra\"\n\
         cli_auth_credentials_store = \"file\"\n\
         suppress_unstable_features_warning = true\nanalytics.enabled = false\n\
         windows.sandbox = \"unelevated\"\n\
         tui.disable_paste_burst = true\n\
         notice.model_migrations.\"gpt-5.6-terra\" = \"gpt-6-sol\"\n\
         {}\n[projects.{}]\ntrust_level = \"{}\"\n",
            if command == "discovered" {
                "tui.status_line=[\"model\",\"thread-id\",\"thread-name\"]\nfeatures.daemon_auto_start=false"
            } else {
                ""
            },
            serde_json::to_string(&workspace_path)?,
            if command == "agents-untrusted" {
                "untrusted"
            } else {
                "trusted"
            },
        ),
    )?;
    let mismatch = command.starts_with("mismatch-");
    let persisted = command == "mismatch-disable-persisted";
    let disabling = matches!(command, "mismatch-disable" | "mismatch-disable-persisted");
    let restart = command == "mismatch-restart" || disabling;
    fs::write(
        home.path().join("auth.json"),
        r#"{"OPENAI_API_KEY":"daemon-startup-test","tokens":null,"last_refresh":null}"#,
    )?;
    let mut env = std::env::vars().collect::<std::collections::HashMap<_, _>>();
    for key in [
        "CODEX_EXEC_SERVER_URL",
        "CODEX_ACCESS_TOKEN",
        "CODEX_API_KEY",
        "OPENAI_API_KEY",
        "OPENAI_FEDERATION_RULE_ID",
        "OPENAI_IDENTITY_TOKEN_FILE",
        "OPENAI_WORKLOAD_IDENTITY_CONTEXT",
    ] {
        env.remove(key);
    }
    env.insert("CODEX_HOME".into(), home.path().display().to_string());
    env.insert(
        "CODEX_SQLITE_HOME".into(),
        home.path().display().to_string(),
    );
    env.insert("TERM".into(), "xterm-256color".into());
    if matches!(command, "resume" | "fork" | "mismatch-embedded") {
        env.insert("CODEX_ALLOW_EMBEDDED_SERVER".into(), "1".into());
    }
    let mut args = Vec::new();
    let mut steps: VecDeque<(&str, &[u8])> = VecDeque::new();
    if matches!(
        command,
        "start" | "agents" | "agents-untrusted" | "discovered" | "restrictive-job"
    ) || mismatch
    {
        // A selected package with a stopped daemon avoids installing a release.
        let managed = home
            .path()
            .join("packages/app-server-daemon/current/bin")
            .join(if cfg!(windows) { "codex.exe" } else { "codex" });
        fs::create_dir_all(home.path().join("packages/app-server-daemon/current/bin"))?;
        // Hard links change the executable's ctime and invalidate Rosetta's translation cache.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&codex, &managed)?;
        #[cfg(not(unix))]
        fs::hard_link(&codex, &managed)
            .or_else(|_| codex_utils_cargo_bin::copy_executable(&codex, &managed))?;
        if command != "restrictive-job" {
            codex_uds::prepare_private_socket_directory(&home.path().join("app-server-daemon"))
                .await?;
            fs::write(
                home.path().join("app-server-daemon/settings.json"),
                if persisted {
                    r#"{"shutdownGraceSeconds":0,"updater":{"autoUpdateEnabled":false},"featureOverrides":{"api_key_model_discovery":true}}"#
                } else {
                    r#"{"shutdownGraceSeconds":0,"updater":{"autoUpdateEnabled":false}}"#
                },
            )?;
        }
    }
    let pid_file = home.path().join("app-server-daemon/daemon.pid");
    let result = async {
        // Discovery requires an already running daemon; auto-start is deliberately disabled.
        let mut existing_daemon = if command == "discovered" || mismatch && !disabling {
            let started = Command::new(&codex)
                .env_clear()
                .envs(&env)
                .current_dir(&workspace_path)
                .args(["app-server", "daemon", "start"])
                .output()?;
            ensure!(
                started.status.success(),
                "{}",
                String::from_utf8_lossy(&started.stderr)
            );
            Some(fs::read(&pid_file)?)
        } else {
            None
        };
        let mut restarted_thread = None;
        let expected = if command == "start" {
            // The draft header is visible before the session's command composer is ready.
            steps.push_back(("GPT-5.6-Terra", b"/status\r"));
            "Server:Localbackgroundserver"
        } else if command == "agents" {
            args.push("agents".into());
            steps.push_back(("Notasksyet", b"\x0e"));
            steps.push_back(("GPT-5.6-Terra", b"/daemon\r"));
            // The fork reports the systemd-managed service, never replaces it from the TUI.
            "Localsharedservice:"
        } else if command == "agents-untrusted" {
            args.push("agents".into());
            steps.push_back(("Notasksyet", b"\x0e"));
            "entercontinue·escback"
        } else if command == "discovered" {
            args.extend(["--disable".into(), "daemon_auto_start".into()]);
            // Empty drafts need not survive a daemon restart. Renaming materializes the
            // thread; wait for the confirmed footer name, not the echoed command.
            steps.push_back(("GPT-5.6-Terra", b"/rename daemon-restart-test\r"));
            // Readiness-only barrier: the harness, never the TUI, restarts its private daemon.
            steps.push_back(("·daemon-restart-test", b""));
            // Offline editing preserves text but suppresses Enter. Observe the command
            // popup in a complete connected frame before submitting the retained draft.
            steps.push_back(("·daemon-restart-test", b"/status"));
            steps.push_back(("/statusshowcurrentsessionconfigurationandtokenusage", b"\r"));
            "Server:Localbackgroundserver"
        } else if elevated_launch || command == "restrictive-job" && elevated {
            if let Some(action) = command.strip_prefix("elevated-") {
                args.push(action.into());
                steps.push_back(("Nosessionsyet", b"\x1b"));
            }
            steps.push_back(("GPT-5.6-Terra", b"\x14"));
            "Runningasadministrator:sharedbackgroundserverdisabled."
        } else if command == "restrictive-job" {
            steps.push_back(("GPT-5.6-Terra", b"\x14"));
            "Runningwithoutthesharedbackgroundserver:thisWindowslauncher"
        } else if mismatch {
            args.extend(if disabling && !persisted {
                ["--disable".into(), "auth_elicitation".into()]
            } else {
                ["--disable".into(), "api_key_model_discovery".into()]
            });
            let input: &[u8] = match command {
                "mismatch-cancel" => b"\x03",
                "mismatch-embedded" => b"1",
                // Disabled embedded fallback has no numeric shortcut; restart is the first enabled row.
                "mismatch-restart" | "mismatch-disable" | "mismatch-disable-persisted" => b"1\r",
                _ => unreachable!(),
            };
            steps.push_back(("Backgroundserverhasincompatiblefeaturesettings", input));
            if command == "mismatch-cancel" {
                "--no-daemon"
            } else {
                steps.push_back(("GPT-5.6-Terra", b"/status\r"));
                if restart { "Server:Localbackgroundserver" } else { "Model:" }
            }
        } else {
            args.extend([command.into(), "--strict-config".into()]);
            if command == "resume" {
                let thread_id = "00000000-0000-0000-0000-000000000001";
                let sessions = home.path().join("sessions/2025/01/05");
                fs::create_dir_all(&sessions)?;
                let metadata = serde_json::json!({
                    "timestamp": "2025-01-05T12:00:00Z",
                    "type": "session_meta",
                    "payload": {
                        "session_id": thread_id, "id": thread_id,
                        "timestamp": "2025-01-05T12:00:00Z", "cwd": workspace_path,
                        "originator": "codex", "cli_version": "0.0.0",
                        "source": "cli", "model_provider": "openai"
                    }
                });
                let message = serde_json::json!({
                    "timestamp": "2025-01-05T12:00:00Z",
                    "type": "event_msg",
                    "payload": {"type": "user_message", "message": "Saved resume session", "kind": "plain"}
                });
                fs::write(
                    sessions.join(format!("rollout-2025-01-05T12-00-00-{thread_id}.jsonl")),
                    format!("{metadata}\n{message}\n"),
                )?;
                steps.push_back(("Savedresumesession", b"\r"));
            } else {
                steps.push_back(("Nosessionsyet", b"\x1b"));
            }
            steps.push_back(("GPT-5.6-Terra", b"\x14"));
            if elevated {
                "Runningasadministrator:sharedbackgroundserverdisabled."
            } else {
                "Runningwithoutthesharedbackgroundserver:--strict-config"
            }
        };
        let program = if cfg!(windows) && command == "restrictive-job" {
            env.insert("CODEX_TEST_RESTRICTIVE_DAEMON_START".into(), "1".into());
            args = vec![
                "--exact".into(),
                "restrictive_launcher_uses_embedded_if_daemon_cannot_start".into(),
                "--nocapture".into(),
            ];
            std::env::current_exe()?
        } else {
            codex.clone()
        };
        let spawned = codex_utils_pty::spawn_pty_process(
            program.as_os_str(),
            &args,
            &workspace_path,
            &env,
            /*arg0*/ &None,
            codex_utils_pty::TerminalSize {
                rows: 40,
                cols: 120,
            },
            codex_utils_pty::ChildFds::Inherited(&[]),
        )
        .await?;
        let total_steps = steps.len();
        let mut waiting_on = "first_pty_output";
        let exit = spawned.exit_rx;
        let session = spawned.session;
        let writer = session.writer_sender();
        let mut stdout = spawned.stdout_rx;
        let mut output = String::new();
        let mut screen = vt100::Parser::new(
            /*rows*/ 40, /*cols*/ 120, /*scrollback_len*/ 0,
        );
        let mut queries: Vec<(&str, &[u8])> = vec![
            ("\x1b[6n", b"\x1b[1;1R"),
            ("\x1b[?u", b"\x1b[?0u\x1b[?1;2c"),
            ("\x1b[c", b"\x1b[?1;2c"),
            ("\x1b]10;?", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
            ("\x1b]11;?", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
        ];
        // A fresh disable request includes both cold daemon startup and its confirmed restart.
        let timeout = Duration::from_secs(if disabling { 90 } else { 45 });
        let result = tokio::time::timeout(timeout, async {
            while let Some(bytes) = stdout.recv().await {
                waiting_on = "ready_text";
                output.push_str(&String::from_utf8_lossy(&bytes));
                screen.process(&bytes);
                for (query, reply) in &mut queries {
                    if !query.is_empty() && output.contains(*query) {
                        writer.send(reply.to_vec()).await?;
                        *query = "";
                    }
                }
                // PTY reads can split a redraw after the reconnect notice is erased but
                // before it is repainted. Only inspect completed synchronized updates.
                if command == "discovered"
                    && output
                        .rfind("\x1b[?2026l")
                        .is_none_or(|end| output[end..].contains("\x1b[?2026h"))
                {
                    waiting_on = "complete_synchronized_frame";
                    continue;
                }
                let text: String = screen
                    .screen()
                    .contents()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                if let Some((ready, input)) = steps.front()
                    && text.contains(ready)
                {
                    if command == "discovered" {
                        if input.is_empty() {
                            let Some(thread_id) = screen
                                .screen()
                                .contents()
                                .split_whitespace()
                                .find_map(|part| codex_protocol::ThreadId::from_string(part).ok())
                            else {
                                waiting_on = "pre_restart_live_thread_uuid";
                                continue;
                            };
                            restarted_thread = Some(thread_id);
                            existing_daemon = Some(fs::read(&pid_file)?);
                            // The fork's /daemon menu is status-only. Exercise the supported
                            // explicit CLI control against this fixture's hermetic daemon.
                            waiting_on = "explicit_cli_restart";
                            let restarted = tokio::process::Command::new(&codex)
                                .env_clear()
                                .envs(&env)
                                .current_dir(&workspace_path)
                                .args(["app-server", "daemon", "restart"])
                                .kill_on_drop(true)
                                .output()
                                .await?;
                            ensure!(
                                restarted.status.success(),
                                "{}",
                                String::from_utf8_lossy(&restarted.stderr)
                            );
                            ensure!(Some(fs::read(&pid_file)?) != existing_daemon);
                            steps.pop_front();
                            output.clear();
                            waiting_on = "post_restart_redraw";
                            continue;
                        }
                        if *input == b"/status" || *input == b"\r" {
                            let thread_id = restarted_thread.context("missing pre-restart thread")?;
                            if !text.contains(&thread_id.to_string())
                                || !text.contains("·daemon-restart-test")
                            {
                                waiting_on = "same_restored_thread_uuid_and_name";
                                continue;
                            }
                        }
                        // Reconnect renders the restored snapshot through clear_for_thread_switch.
                        // Owned fullscreen clears AfterCursor (CSI J); only inline purges with CSI 3J.
                        // Require the owned-screen rebuild receipt before probing the composer.
                        if *input == b"/status" && !output.contains("\x1b[J") {
                            waiting_on = "owned_screen_rebuild_csi_j";
                            continue;
                        }
                        // The old header can linger while the daemon stops. Check both
                        // the draft and its Enter against the replacement process and UI.
                        if *input == b"/status" || *input == b"\r" {
                            if !fs::read(&pid_file).is_ok_and(|pid| Some(pid) != existing_daemon) {
                                waiting_on = "replacement_daemon_pid";
                                continue;
                            }
                            if text.contains("Reconnecting") {
                                waiting_on = "connected_frame_without_reconnecting";
                                continue;
                            }
                        }
                    }
                    if disabling && *ready == "Backgroundserverhasincompatiblefeaturesettings" {
                        existing_daemon = Some(fs::read(&pid_file)?);
                        let settings: serde_json::Value = serde_json::from_slice(&fs::read(home.path().join("app-server-daemon/settings.json"))?)?;
                        if persisted {
                            ensure!(settings["featureOverrides"]["api_key_model_discovery"] == true);
                        } else {
                            ensure!(settings["featureOverrides"]["auth_elicitation"].is_null());
                        }
                    }
                    writer.send(input.to_vec()).await?;
                    steps.pop_front();
                    output.clear();
                    waiting_on = "next_step_or_expected_status";
                } else if steps.is_empty() && text.contains(expected) {
                    if mismatch {
                        let previous_pid = existing_daemon.as_ref().context("missing original daemon PID")?;
                        ensure!((fs::read(&pid_file)? != *previous_pid) == restart);
                        if command == "mismatch-cancel" {
                            ensure!(text.contains("Cannotusethesharedbackgroundserver:Thissessionrequiresapi_key_model_discoverytobedisabled."));
                        } else {
                            ensure!(text.contains("Server:Localbackgroundserver") == restart);
                            if restart {
                                let settings: serde_json::Value = serde_json::from_slice(
                                    &fs::read(home.path().join("app-server-daemon/settings.json"))?
                                )?;
                                let flag = if disabling && !persisted {
                                    "auth_elicitation"
                                } else {
                                    "api_key_model_discovery"
                                };
                                assert_eq!(settings["featureOverrides"], serde_json::json!({(flag): false}));
                            }
                        }
                    } else if command == "agents" {
                        ensure!(text.contains("Close"));
                        ensure!(!text.contains("Restartbackgroundserver"));
                    } else if command == "start" {
                        ensure!(text.contains("Server:Localbackgroundserver"));
                        ensure!(home.path().join("app-server-daemon/daemon.pid").exists());
                    } else if command == "discovered" {
                        let previous_pid = existing_daemon.as_ref().context("missing original daemon PID")?;
                        ensure!(fs::read(&pid_file)? != *previous_pid);
                        ensure!(text.contains("Server:Localbackgroundserver"));
                        let thread_id = restarted_thread.context("missing pre-restart thread")?;
                        ensure!(text.contains(&thread_id.to_string()));
                        ensure!(text.contains("·daemon-restart-test"));
                        ensure!(!text.contains("Reconnecting"));
                    } else if let Some(existing_daemon) = &existing_daemon {
                        ensure!(fs::read(&pid_file)? == *existing_daemon);
                    } else if elevated_launch {
                        ensure!(!home.path().join("app-server-daemon").exists());
                        ensure!(!home.path().join("packages/app-server-daemon").exists());
                    } else if command == "restrictive-job" {
                        ensure!(!pid_file.exists());
                        if !elevated {
                            let contents = screen.screen().contents();
                            let warning = contents.lines()
                                .find(|line| line.contains("Running without the shared background server:"))
                                .context("missing rendered fallback warning")?;
                            insta::assert_snapshot!("restrictive_launcher_warning", warning.trim());
                        }
                    }
                    return Ok::<_, anyhow::Error>(());
                }
            }
            anyhow::bail!(
                "TUI exited before {expected}: {}",
                screen.screen().contents()
            )
        })
        .await;
        if command == "mismatch-cancel" && matches!(result, Ok(Ok(()))) {
            let status = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), exit).await??;
            ensure!(status != 0, "incompatible daemon launch must fail");
        }
        Ok::<_, anyhow::Error>((
            session,
            result.with_context(|| {
                let contents = screen.screen().contents();
                let text: String = contents.chars().filter(|c| !c.is_whitespace()).collect();
                let actual_thread = contents
                    .split_whitespace()
                    .find_map(|part| codex_protocol::ThreadId::from_string(part).ok());
                let actual_name = contents
                    .lines()
                    .find(|line| line.trim_start().starts_with("GPT-5.6-Terra"))
                    .and_then(|line| line.split('·').nth(2))
                    .map(str::trim);
                let actual_pid = fs::read(&pid_file).ok();
                let ready = steps.front().map_or(expected, |(ready, _)| *ready);
                let frame_complete = output
                    .rfind("\x1b[?2026l")
                    .is_some_and(|end| !output[end..].contains("\x1b[?2026h"));
                format!(
                    "{command} failed: phase={waiting_on}, step={}/{total_steps}, \
                     ready={ready:?}, ready_present={}, expected_status={expected:?}, status_present={}, \
                     expected_uuid={restarted_thread:?}, actual_uuid={actual_thread:?}, \
                     expected_name=daemon-restart-test, actual_name={actual_name:?}, name_present={}, footer={:?}, \
                     previous_pid={:?}, actual_pid={:?}, pid_changed={}, \
                     frame_complete={frame_complete}, sync_begin={}, sync_end={}, \
                     owned_clear_csi_j={}, inline_purge_csi_3j={}, reconnecting={}\n{contents}",
                    total_steps - steps.len(),
                    text.contains(ready),
                    text.contains(expected),
                    text.contains("·daemon-restart-test"),
                    contents.lines().last(),
                    existing_daemon.as_deref().map(String::from_utf8_lossy),
                    actual_pid.as_deref().map(String::from_utf8_lossy),
                    actual_pid.is_some() && actual_pid != existing_daemon,
                    output.contains("\x1b[?2026h"),
                    output.contains("\x1b[?2026l"),
                    output.contains("\x1b[J"),
                    output.contains("\x1b[3J"),
                    text.contains("Reconnecting"),
                )
            }),
        ))
    }
    .await;
    // Stop the detached daemon, including on timeout or failed assertions.
    let stopped = Command::new(&codex)
        .env_clear()
        .envs(&env)
        .args(["app-server", "daemon", "stop"])
        .output()?;
    let (session, result) = result?;
    session.terminate();
    result??;
    ensure!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    Ok(())
}
