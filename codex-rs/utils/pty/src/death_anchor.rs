//! Parent-death signals follow the forking thread, not the process.
//!
//! The kernel delivers `PR_SET_PDEATHSIG` when the thread that forked the
//! child exits (prctl(2)). Tokio worker and blocking-pool threads retire while
//! Codex keeps running, so a child forked from one of them receives a stray
//! `SIGTERM` in the middle of its command. Every spawn that arms the signal
//! therefore forks from one anchor thread that lives as long as the process.

use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::LazyLock;
use std::sync::mpsc;

type Job = Box<dyn FnOnce() + Send>;

static ANCHOR: LazyLock<Result<mpsc::Sender<Job>, String>> = LazyLock::new(|| {
    let (jobs, queue) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("codex-spawn-anchor".to_string())
        .spawn(move || {
            for job in queue {
                job();
            }
        })
        .map(|_| jobs)
        .map_err(|error| error.to_string())
});

/// Runs `spawn` on the anchor thread, inside the caller's Tokio runtime when
/// there is one, and returns its result. A panic in `spawn` resumes in the
/// caller; the anchor survives it.
pub fn anchored<T: Send + 'static>(
    spawn: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let jobs = ANCHOR
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))?;
    let runtime = tokio::runtime::Handle::try_current().ok();
    let (reply, outcome) = mpsc::sync_channel(1);
    jobs.send(Box::new(move || {
        let _entered = runtime.as_ref().map(tokio::runtime::Handle::enter);
        let _ = reply.send(std::panic::catch_unwind(AssertUnwindSafe(spawn)));
    }))
    .map_err(|_| io::Error::other("spawn anchor thread exited"))?;
    match outcome.recv() {
        Ok(Ok(result)) => result,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => Err(io::Error::other("spawn anchor thread exited")),
    }
}
