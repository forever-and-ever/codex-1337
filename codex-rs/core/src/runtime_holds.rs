//! Runtime custody for automatic unloading, not a claim about every OS descendant.
//! Backend-owned processes through output settlement, executing/yielded Code Mode cells,
//! armed alarms and admitted runtime users pin their exact runtime. Fully detached
//! daemons, connected MCP servers and post-turn MCP jobs or async hooks do not.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::watch;

pub(crate) enum IdleSeal {
    Sealed,
    Busy,
    Contended,
}

pub(crate) const DEFAULT_THREAD_UNLOAD_DELAY_SECS: u64 = 1800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeActivity {
    Held { count: NonZeroUsize, revision: u64 },
    Quiet { since: Instant, revision: u64 },
    Closing,
}

impl RuntimeActivity {
    fn next_revision(self) -> u64 {
        let revision = match self {
            Self::Held { revision, .. } | Self::Quiet { revision, .. } => revision,
            Self::Closing => unreachable!("closing runtimes cannot admit work"),
        };
        revision
            .checked_add(1)
            .unwrap_or_else(|| unreachable!("runtime activity revision exhausted"))
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeHolds(Arc<watch::Sender<RuntimeActivity>>);

#[derive(Clone, Debug)]
pub struct WeakRuntimeHolds(std::sync::Weak<watch::Sender<RuntimeActivity>>);

impl WeakRuntimeHolds {
    pub fn upgrade(&self) -> Option<RuntimeHolds> {
        self.0.upgrade().map(RuntimeHolds)
    }
}

impl RuntimeHold {
    pub(crate) fn settlement(&self) -> tokio_util::sync::CancellationToken {
        self.1.clone()
    }
}

impl RuntimeHolds {
    pub fn same_runtime(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn downgrade(&self) -> WeakRuntimeHolds {
        WeakRuntimeHolds(Arc::downgrade(&self.0))
    }
}

impl Default for RuntimeHolds {
    fn default() -> Self {
        Self(Arc::new(
            watch::channel(RuntimeActivity::Quiet {
                since: Instant::now(),
                revision: 0,
            })
            .0,
        ))
    }
}

/// Dropping a controller is not settlement: the actual work owner retains this guard.
#[derive(Debug)]
pub struct RuntimeHold(RuntimeHolds, tokio_util::sync::CancellationToken);

impl RuntimeHolds {
    pub fn subscribe(&self) -> watch::Receiver<RuntimeActivity> {
        self.0.subscribe()
    }

    pub fn activity(&self) -> RuntimeActivity {
        *self.0.borrow()
    }

    pub fn acquire(&self) -> Option<RuntimeHold> {
        self.0
            .send_if_modified(|state| {
                let count = match *state {
                    RuntimeActivity::Held { count, .. } => count.get(),
                    RuntimeActivity::Quiet { .. } => 0,
                    RuntimeActivity::Closing => return false,
                };
                *state = RuntimeActivity::Held {
                    count: NonZeroUsize::new(
                        count
                            .checked_add(1)
                            .unwrap_or_else(|| unreachable!("runtime holds exhausted")),
                    )
                    .unwrap_or_else(|| unreachable!("acquisition is nonzero")),
                    revision: state.next_revision(),
                };
                true
            })
            .then(|| RuntimeHold(self.clone(), tokio_util::sync::CancellationToken::new()))
    }

    pub(crate) fn close(&self) {
        self.0.send_replace(RuntimeActivity::Closing);
    }

    pub(crate) fn seal(&self, expected_revision: u64) -> bool {
        self.0.send_if_modified(|state| {
            if matches!(*state, RuntimeActivity::Quiet { revision, .. } if revision == expected_revision) {
                *state = RuntimeActivity::Closing;
                true
            } else {
                false
            }
        })
    }

    pub(crate) fn is_closing(&self) -> bool {
        matches!(self.activity(), RuntimeActivity::Closing)
    }
}

impl Drop for RuntimeHold {
    fn drop(&mut self) {
        self.0.0.send_if_modified(|state| {
            let RuntimeActivity::Held { count, .. } = *state else {
                assert!(
                    matches!(state, RuntimeActivity::Closing),
                    "runtime hold lost custody"
                );
                return false;
            };
            let revision = state.next_revision();
            *state = match NonZeroUsize::new(count.get() - 1) {
                Some(count) => RuntimeActivity::Held { count, revision },
                None => RuntimeActivity::Quiet {
                    since: Instant::now(),
                    revision,
                },
            };
            true
        });
        self.1.cancel();
    }
}
