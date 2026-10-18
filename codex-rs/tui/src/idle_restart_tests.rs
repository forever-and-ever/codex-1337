use super::*;
use clap::Parser;
use futures::FutureExt;
use pretty_assertions::assert_eq;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::process::Command;
use std::process::Stdio;

#[tokio::test(start_paused = true)]
async fn quiet_interval_and_installed_generation_gate_replacement() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let distributions = root.path().join("distributions");
    for version in ["old", "unsupported", "new"] {
        std::fs::create_dir_all(distributions.join(version).join("bin"))?;
        std::fs::write(distributions.join(version).join("distribution"), "fixture")?;
        let body = if version == "new" {
            format!("#!/bin/sh\nprintf '%s\\n' '{IDLE_RESTART_PROTOCOL}'\n")
        } else {
            "#!/bin/sh\nexit 2\n".into()
        };
        let executable = distributions.join(version).join("bin/codex");
        std::fs::write(&executable, body)?;
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755))?;
    }
    let running = distributions.join("old/bin/codex");
    let current = distributions.join("current");
    symlink("old", &current)?;
    let mut restart = IdleRestart {
        installed: installed_entrypoint(&running),
        running,
        deadline: None,
        rejected: None,
    };
    restart.set_ready(/*ready*/ false);
    tokio::time::advance(IDLE * 2).await;
    assert!(restart.tick().now_or_never().is_none());
    // A long-running turn becoming idle still earns a full five-minute interval.
    restart.set_ready(/*ready*/ true);
    tokio::time::advance(IDLE.checked_sub(Duration::from_secs(/*secs*/ 1)).unwrap()).await;
    assert!(restart.tick().now_or_never().is_none());
    // Fresh user activity restarts the entire quiet interval.
    restart.postpone();
    tokio::time::advance(IDLE.checked_sub(Duration::from_secs(/*secs*/ 1)).unwrap()).await;
    assert!(restart.tick().now_or_never().is_none());
    tokio::time::advance(Duration::from_secs(/*secs*/ 1)).await;
    assert_eq!(restart.tick().now_or_never(), Some(()));
    tokio::time::resume();
    assert_eq!(restart.replacement().await, None);

    std::fs::remove_file(&current)?;
    symlink("unsupported", &current)?;
    assert_eq!(restart.replacement().await, None);
    assert_eq!(
        restart.rejected,
        Some(distributions.join("unsupported/bin/codex"))
    );
    std::fs::remove_file(&current)?;
    symlink("new", &current)?;
    let replacement = distributions.join("new/bin/codex");
    assert_eq!(restart.replacement().await, Some(replacement.clone()));
    // Input arriving during the probe postpones, rather than blacklisting, a build.
    restart.reject(replacement.clone(), &io::ErrorKind::WouldBlock.into());
    assert_eq!(restart.replacement().await, Some(replacement.clone()));
    // Hosted admission timeouts are rejections, not retries on each quiet interval.
    restart.reject(replacement.clone(), &io::ErrorKind::TimedOut.into());
    assert_eq!(restart.replacement().await, None);
    Ok(())
}

#[test]
#[expect(
    clippy::print_stdout,
    reason = "The re-exec child reports its PID over the test pipe."
)]
fn reexec_keeps_pid_and_resumes_once_without_replaying_launch_input() -> anyhow::Result<()> {
    const CHILD: &str = "CODEX_TEST_IDLE_REEXEC";
    if let Ok(mode) = std::env::var(CHILD) {
        let thread = ThreadId::from_string("00000000-0000-4000-8000-000000000001")?;
        if std::env::var_os(HANDOFF_ENV).is_none() {
            let error = reexec(
                &std::env::current_exe()?,
                thread,
                AbsolutePathBuf::from_absolute_path(std::env::current_dir()?)?,
            );
            return Err(error.into());
        }
        let mut cli = Cli::parse_from([
            "codex",
            "--model",
            "retained-model",
            "--prompt",
            "original prompt",
        ]);
        cli.fork_session_id = Some("original-fork".into());
        cli.agents_overview = true;
        cli.images.push(PathBuf::from("original-image.png"));
        apply_handoff(&mut cli)?;
        if mode == "foreign" {
            assert!(!rejoins(thread));
            assert_eq!(cli.resume_session_id, None);
            assert_eq!(cli.prompt.as_deref(), Some("original prompt"));
        } else {
            assert!(rejoins(thread));
            assert_eq!(
                (
                    cli.resume_session_id.as_deref(),
                    cli.prompt.as_deref(),
                    cli.images.is_empty(),
                    cli.fork_session_id.as_deref(),
                    cli.agents_overview,
                    cli.model.as_deref()
                ),
                (
                    Some(thread.to_string().as_str()),
                    None,
                    true,
                    None,
                    false,
                    Some("retained-model")
                )
            );
            assert_eq!(cli.cwd, Some(std::env::current_dir()?));
            let mut second = Cli::parse_from(["codex", "--prompt", "a new launch"]);
            apply_handoff(&mut second)?;
            assert_eq!(second.resume_session_id, None);
            assert_eq!(second.prompt.as_deref(), Some("a new launch"));
        }
        println!("\nREEXEC_PID={}", std::process::id());
        return Ok(());
    }

    let root = tempfile::tempdir()?;
    for mode in ["replace", "foreign"] {
        let mut command = Command::new(std::env::current_exe()?);
        let _ = command.args(["--exact", "idle_restart::tests::reexec_keeps_pid_and_resumes_once_without_replaying_launch_input", "--nocapture"])
            .current_dir(root.path()).env(CHILD, mode).env_remove(HANDOFF_ENV)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if mode == "foreign" {
            let _ = command.env(
                HANDOFF_ENV,
                serde_json::to_string(&Handoff {
                    pid: std::process::id(),
                    thread: ThreadId::new(),
                    cwd: AbsolutePathBuf::from_absolute_path(root.path())?,
                })?,
            );
        }
        let child = command.spawn()?;
        let pid = child.id();
        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == format!("REEXEC_PID={pid}"))
        );
    }
    Ok(())
}
