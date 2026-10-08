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
    // The v1 transport marker predates `unstarted`: it cannot establish that a
    // strict legacy reader accepts that field, even when its value is null.
    for (versions, checkpoint, expected) in [
        ("exit 2", serde_json::json!({}), None),
        (
            "exit 2",
            serde_json::json!({"unstarted": null}),
            Some(io::ErrorKind::Other),
        ),
        (
            "printf '[1,2]\\n'",
            serde_json::json!({"unstarted": null}),
            None,
        ),
        ("printf '[1,2,3]\\n'", serde_json::json!({}), None),
        (
            "printf '[1]\\n'",
            serde_json::json!({"unstarted": null}),
            Some(io::ErrorKind::Other),
        ),
        (
            "printf '[2,1]\\n'",
            serde_json::json!({}),
            Some(io::ErrorKind::Other),
        ),
        (
            "printf '[1,2]\\nnoise'",
            serde_json::json!({}),
            Some(io::ErrorKind::Other),
        ),
        (
            "head -c 257 /dev/zero",
            serde_json::json!({}),
            Some(io::ErrorKind::Other),
        ),
        (
            "exec sleep 30",
            serde_json::json!({}),
            Some(io::ErrorKind::TimedOut),
        ),
    ] {
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\n[ \"$1\" = debug ] && [ \"$2\" = tui-reload-probe ] || exit 77\nif [ \"$3\" = --checkpoint-versions ]; then\n{versions}\nelse\nprintf '%s\\n' '{}'\nfi\n",
                super::super::retained::PROTOCOL,
            ),
        )?;
        assert_eq!(
            super::super::probe::admit(
                &executable,
                super::super::retained::checkpoint_version(&checkpoint)?
            )
            .await
            .err()
            .map(|error| error.kind()),
            expected,
        );
    }
    Ok(())
}
