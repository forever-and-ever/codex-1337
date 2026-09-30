use std::time::Duration;

use pretty_assertions::assert_eq;

use super::CommandBackend;

// Desktop actions can remain pending while the TUI processes input. User text is argv, not shell.
#[tokio::test]
async fn pending_action_does_not_block_and_preserves_literal_message() {
    let scratch = tempfile::tempdir().unwrap();
    let message_path = scratch.path().join("message");
    let release_path = scratch.path().join("release");
    let done_path = scratch.path().join("done");
    let message = "Question: $(exit 93) 'α' \"<&>\"\nnext line";
    let backend = CommandBackend(vec![
        "/bin/sh".into(),
        "-c".into(),
        "set -eu; printf '%s' \"$4\" > \"$1.tmp\"; mv \"$1.tmp\" \"$1\"; while ! test -e \"$2\"; do sleep 0.02; done; touch \"$3\"".into(),
        "notification-test".into(),
        message_path.to_string_lossy().into_owned(),
        release_path.to_string_lossy().into_owned(),
        done_path.to_string_lossy().into_owned(),
    ]);
    backend.notify(message).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !message_path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(std::fs::read_to_string(&message_path).unwrap(), message);
        assert!(!done_path.exists());
        std::fs::write(&release_path, "").unwrap();
        while !done_path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
