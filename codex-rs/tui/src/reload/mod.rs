//! Replaceable UI workers. User work belongs to a versioned checkpoint; paint does not.
//!
//! The supervisor's reload deadline must never await worker shutdown, server I/O,
//! checkpoint encoding, or process creation. Those operations run outside its reactor.
//! Checkpoints protect the handoff, not arbitrary crashes after it commits. A ready
//! worker owns its input again; the host discards the old snapshot rather than replaying
//! stale queues after a later crash. `PROTOCOL` identifies the stable transport,
//! not every checkpoint extension. An opt-in probe advertises readable checkpoint
//! versions; legacy probes establish only the original extension-free schema.
//! Keep old readers when adding a schema so existing supervisors can upgrade.
//! Admission probes run before retiring the writer, with their own bounded budget;
//! the existing handoff/rollback deadline begins only after admission succeeds.
//! Reload replies have no independent transport timer: only the supervisor may
//! reject the snapshot or retire its writer, never a client that merely timed out.
//!
//! Idle rejection handoff is opt-in on Hello, without changing checkpoint semantics.
//! A supporting host wraps State with the last rejected canonical candidate; only
//! then may a worker wrap an automatic Reload with the exact candidate it observed.
//! Legacy peers still exchange bare checkpoints. Directional wrappers keep attempted
//! and rejected installations distinct, and avoid leaking host state to shell tools
//! through the environment. Rollback records rejection before launching the old UI.

mod deadline;
mod retained;
pub use retained::PROTOCOL;
pub use retained::READABLE_CHECKPOINT_VERSIONS;
pub(crate) use retained::RetainedInput;
pub(crate) use retained::RetainedSessionDraft;
#[cfg(target_os = "linux")]
mod admission;
#[cfg(target_os = "linux")]
mod broker;
#[cfg(target_os = "linux")]
mod control;
#[cfg(target_os = "linux")]
mod host;
#[cfg(target_os = "linux")]
mod probe;
#[cfg(target_os = "linux")]
mod terminal;
#[cfg(target_os = "linux")]
pub(crate) mod worker;
#[cfg(target_os = "linux")]
mod worker_process;

#[cfg(target_os = "linux")]
pub async fn run_host() -> std::io::Result<i32> {
    host::run().await
}
