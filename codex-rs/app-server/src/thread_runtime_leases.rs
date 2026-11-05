//! Exact-runtime custody for viewers and headless callers. Pending intents survive
//! an unload fence without retaining the runtime being replaced.

use super::ListenerStateCandidate;
use super::ThreadEntry;
use super::ThreadStateManagerInner;
use crate::outgoing_message::ConnectionId;
use codex_core::CodexThread;
use codex_core::runtime_holds::RuntimeHold;
use codex_core::runtime_holds::RuntimeHolds;
use codex_protocol::ThreadId;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::time::Instant;
use tokio::sync::watch;

#[derive(Clone, Copy)]
pub(crate) enum SubscriberActivity {
    Attached,
    Detached { since: Instant },
}
impl SubscriberActivity {
    pub(super) fn detached() -> Self {
        Self::Detached {
            since: Instant::now(),
        }
    }
    fn update(&mut self, viewed: bool) -> bool {
        match (*self, viewed) {
            (Self::Attached, true) | (Self::Detached { .. }, false) => false,
            (_, true) => {
                *self = Self::Attached;
                true
            }
            (_, false) => {
                *self = Self::detached();
                true
            }
        }
    }
}
pub(crate) enum ListenerBinding<T> {
    Installed {
        listener: T,
        has_subscribers: watch::Receiver<bool>,
        attached: bool,
    },
    Stale,
    Closing,
}
#[derive(Clone, Copy)]
pub(crate) enum ListenerInstallMode {
    Live,
    Created,
}
#[derive(Debug)]
struct RuntimeClosing;
struct RuntimeAdmission {
    runtime: RuntimeHolds,
    hold: RuntimeHold,
}

enum ListenerAdmission {
    Live(RuntimeAdmission),
    StoppedDrain,
}
impl RuntimeAdmission {
    fn bind(runtime: &RuntimeHolds) -> Result<Self, RuntimeClosing> {
        let hold = runtime.acquire().ok_or(RuntimeClosing)?;
        Ok(Self {
            runtime: runtime.clone(),
            hold,
        })
    }
}

enum RuntimeUse {
    Pending,
    Bound {
        runtime: RuntimeHolds,
        _hold: RuntimeHold,
    },
}

/// Pins the selected runtime for one server operation without granting a
/// subscription or authority to answer approvals.
pub(crate) struct ThreadRuntimeLease(Arc<Mutex<RuntimeUse>>);

impl ThreadRuntimeLease {
    pub(crate) fn bind(&self, thread: &CodexThread) -> bool {
        Self::bind_intent(&self.0, &thread.runtime_holds())
    }

    fn bind_intent(intent: &Mutex<RuntimeUse>, runtime: &RuntimeHolds) -> bool {
        let mut intent = intent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let RuntimeUse::Bound {
            runtime: current, ..
        } = &*intent
            && current.same_runtime(runtime)
        {
            return !matches!(
                runtime.activity(),
                codex_core::runtime_holds::RuntimeActivity::Closing
            );
        }
        let Some(hold) = runtime.acquire() else {
            return false;
        };
        *intent = RuntimeUse::Bound {
            runtime: runtime.clone(),
            _hold: hold,
        };
        true
    }
}

#[derive(Default)]
enum RuntimeBinding {
    #[default]
    Unbound,
    Bound {
        runtime: RuntimeHolds,
        viewer: Option<RuntimeHold>,
    },
}

#[derive(Default)]
pub(super) struct RuntimeLeases {
    binding: RuntimeBinding,
    intents: Vec<Weak<Mutex<RuntimeUse>>>,
}

impl RuntimeLeases {
    pub(super) fn intent(&mut self) -> ThreadRuntimeLease {
        self.intents.retain(|intent| intent.strong_count() != 0);
        let intent = Arc::new(Mutex::new(RuntimeUse::Pending));
        if let RuntimeBinding::Bound { runtime, .. } = &self.binding {
            ThreadRuntimeLease::bind_intent(&intent, runtime);
        }
        self.intents.push(Arc::downgrade(&intent));
        ThreadRuntimeLease(intent)
    }

    fn bind(&mut self, runtime: &RuntimeHolds, viewed: bool) -> Result<(), RuntimeClosing> {
        let admission = RuntimeAdmission::bind(runtime)?;
        self.commit(admission, viewed);
        Ok(())
    }

    fn commit(&mut self, admission: RuntimeAdmission, viewed: bool) {
        let RuntimeAdmission { runtime, hold } = admission;
        // Retain admission through the synchronous install transaction and transfer
        // its exact hold to the viewer without a release/acquire gap.
        self.intents.retain(|intent| {
            if let Some(intent) = intent.upgrade() {
                let pending = matches!(
                    *intent
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    RuntimeUse::Pending
                );
                if pending {
                    ThreadRuntimeLease::bind_intent(&intent, &runtime);
                }
                true
            } else {
                false
            }
        });
        self.binding = RuntimeBinding::Bound {
            runtime,
            viewer: viewed.then_some(hold),
        };
    }

    pub(super) fn viewers(&mut self, viewed: bool) -> bool {
        if let RuntimeBinding::Bound { runtime, viewer } = &mut self.binding {
            if viewed {
                let Some(hold) = runtime.acquire() else {
                    return false;
                };
                if viewer.is_none() {
                    *viewer = Some(hold);
                }
            } else {
                *viewer = None;
            }
        }
        true
    }
}

struct RuntimeLeaseContributor(super::ThreadStateManager);

impl codex_extension_api::ThreadLifecycleContributor<codex_core::config::Config>
    for RuntimeLeaseContributor
{
    #[expect(
        clippy::expect_used,
        reason = "core validates thread IDs and installs canonical runtime custody before contributors; missing custody must fail closed"
    )]
    fn on_thread_start<'a>(
        &'a self,
        input: codex_extension_api::ThreadStartInput<'a, codex_core::config::Config>,
    ) -> codex_extension_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let id = ThreadId::from_string(input.thread_store.level_id())
                .expect("core thread extension data has a validated thread ID");
            let runtime = input
                .thread_store
                .get::<RuntimeHolds>()
                .expect("core installs runtime custody before lifecycle contributors");
            let mut state = self.0.state.lock().await;
            if let Some(entry) = state.threads.get_mut(&id) {
                entry
                    .runtime_leases
                    .bind(&runtime, !entry.connection_ids.is_empty())
                    .expect("new runtime must admit its pre-existing host custody");
            }
        })
    }
}

impl ThreadEntry {
    pub(super) fn update_subscriber_activity(&mut self) {
        let viewed = !self.connection_ids.is_empty();
        // Admission precedes insertion; mutation sites only release obsolete custody.
        if !viewed {
            self.runtime_leases.viewers(false);
        }
        self.subscriber_activity
            .send_if_modified(|activity| activity.update(viewed));
    }
}

impl super::ThreadStateManager {
    pub(crate) fn install_runtime_leases(
        &self,
        builder: &mut codex_extension_api::ExtensionRegistryBuilder<codex_core::config::Config>,
    ) {
        builder.thread_lifecycle_contributor(Arc::new(RuntimeLeaseContributor(self.clone())));
    }

    pub(crate) fn try_subscriber_activity(
        &self,
        thread_id: ThreadId,
    ) -> Result<SubscriberActivity, tokio::sync::TryLockError> {
        let state = self.state.try_lock()?;
        Ok(state.threads.get(&thread_id).map_or(
            SubscriberActivity::Detached {
                since: Instant::now(),
            },
            |entry| *entry.subscriber_activity.borrow(),
        ))
    }
}

impl super::ThreadStateManager {
    pub(crate) async fn lease_runtime(&self, thread_id: ThreadId) -> ThreadRuntimeLease {
        self.state
            .lock()
            .await
            .threads
            .entry(thread_id)
            .or_default()
            .runtime_leases
            .intent()
    }
    pub(crate) async fn subscriber_activity(
        &self,
        thread_id: ThreadId,
    ) -> watch::Receiver<SubscriberActivity> {
        self.state
            .lock()
            .await
            .threads
            .entry(thread_id)
            .or_default()
            .subscriber_activity
            .subscribe()
    }
    /// Publishes a new state only when listener installation succeeds, or reuses a current state.
    /// The caller must hold the candidate's state lock.
    pub(crate) async fn subscribe_and_install_listener<T, E>(
        &self,
        thread_id: ThreadId,
        candidate: &ListenerStateCandidate,
        runtime: &RuntimeHolds,
        connection_ids: &[ConnectionId],
        mode: ListenerInstallMode,
        install: impl FnOnce(bool) -> Result<T, E>,
    ) -> Result<ListenerBinding<T>, E> {
        let mut state = self.state.lock().await;
        let ThreadStateManagerInner {
            live_connections,
            threads,
            thread_ids_by_connection,
        } = &mut *state;
        let slot = threads.entry(thread_id);
        match (&slot, candidate) {
            (Entry::Occupied(entry), _) if Arc::ptr_eq(&entry.get().state, candidate.state()) => {}
            (Entry::Vacant(_), ListenerStateCandidate::Unpublished(_)) => {}
            _ => return Ok(ListenerBinding::Stale),
        }
        let attached = connection_ids
            .iter()
            .any(|connection_id| live_connections.contains_key(connection_id));
        // Identity is not admission. Acquire custody before installing or
        // publishing any subscription, atomically against core's capacity seal.
        let admission = match RuntimeAdmission::bind(runtime) {
            Ok(admission) => ListenerAdmission::Live(admission),
            Err(RuntimeClosing) if matches!(mode, ListenerInstallMode::Created) && !attached => {
                ListenerAdmission::StoppedDrain
            }
            Err(RuntimeClosing) => return Ok(ListenerBinding::Closing),
        };
        let installed = install(attached)?;
        let entry = slot.or_insert_with(|| ThreadEntry::new(Arc::clone(candidate.state())));
        match admission {
            ListenerAdmission::Live(admission) => entry
                .runtime_leases
                .commit(admission, attached || !entry.connection_ids.is_empty()),
            ListenerAdmission::StoppedDrain => {
                // Existing viewers may retain a terminal relay, but Closing
                // never grants a new subscription or runtime-use lease.
                entry.runtime_leases.binding = RuntimeBinding::Bound {
                    runtime: runtime.clone(),
                    viewer: None,
                };
            }
        }
        for &connection_id in connection_ids {
            if live_connections.contains_key(&connection_id) {
                thread_ids_by_connection
                    .entry(connection_id)
                    .or_default()
                    .insert(thread_id);
                entry.connection_ids.insert(connection_id);
                self.subscription_lifetimes.insert(connection_id, thread_id);
            }
        }
        entry.update_has_connections();
        Ok(ListenerBinding::Installed {
            listener: installed,
            has_subscribers: entry.has_connections_watcher.subscribe(),
            attached,
        })
    }
}
