//! Blocking preparation must never hold up command execution past its caller timeout.

use super::super::backend_tests::sandbox_request;
use super::CHECK_TIMEOUT;
use super::run_checks_with_metrics;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::time::Duration;

#[test]
fn preparation_timeout_does_not_wait_for_blocking_worker() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = AbsolutePathBuf::from_absolute_path(temp.path())?.canonicalize()?;
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry::new(root.clone().into(), FileSystemAccessMode::Write),
        FileSystemSandboxEntry::new(root.join("protected").into(), FileSystemAccessMode::Read),
    ]);
    let request = sandbox_request(&root, &policy);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        tokio::time::pause();
        // Occupy the only worker; dropping release also unblocks it on assertion failure.
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (started, ready) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = started.send(());
            let _ = wait.recv();
        });
        ready.await?;
        let mut check = Box::pin(run_checks_with_metrics((&request).into(), None));
        assert!(futures::poll!(&mut check).is_pending());
        tokio::time::advance(CHECK_TIMEOUT + Duration::from_millis(1)).await;
        assert!(futures::poll!(&mut check).is_ready());
        drop(release);
        blocker.await?;
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(())
}
