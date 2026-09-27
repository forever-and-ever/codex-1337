//! Replaceable UI workers. User work belongs to a versioned checkpoint; paint does not.
//!
//! The supervisor's reload deadline must never await worker shutdown, server I/O,
//! checkpoint encoding, or process creation. Those operations run outside its reactor.
//! Checkpoints protect the handoff, not arbitrary crashes after it commits. A ready
//! worker owns its input again; the host discards the old snapshot rather than replaying
//! stale queues after a later crash. Bump `PROTOCOL` when retained-state semantics
//! or the host capabilities required by a worker change.

mod deadline;
mod retained;
pub use retained::PROTOCOL;
pub(crate) use retained::RetainedInput;
#[cfg(target_os = "linux")]
mod broker;
#[cfg(target_os = "linux")]
mod control;
#[cfg(target_os = "linux")]
mod host;
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
