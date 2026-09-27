use super::*;
use pretty_assertions::assert_eq;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn incompatible_or_stalled_probe_never_runs_launch_input() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("candidate");
    let slots = Arc::default();
    let mut launch = Launch {
        executable: executable.clone(),
        args: vec!["original-user-prompt".into()],
        directory: directory.path().to_owned(),
        generation: 2,
        size: (80, 24),
        slot: Slot::reserve(&slots).expect("empty slots"),
        reaper: Reaper::new()?,
    };
    // A pre-host binary must not receive the original prompt/fork command line.
    for (probe, expected) in [
        (
            "printf 'an-old-protocol-that-is-not-ours\\n'".to_owned(),
            Some(io::ErrorKind::Other),
        ),
        ("exec sleep 30".to_owned(), Some(io::ErrorKind::TimedOut)),
        (
            format!("printf '{}\\n'", super::super::retained::PROTOCOL),
            None,
        ),
    ] {
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\n[ \"$1\" = debug ] && [ \"$2\" = tui-reload-probe ] || exit 77\n{probe}\n"
            ),
        )?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        assert_eq!(
            launch.negotiate().await.err().map(|error| error.kind()),
            expected
        );
    }
    Ok(())
}
