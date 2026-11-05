//! Exact-runtime custody for viewers and headless callers. Pending intents survive
//! an unload fence without retaining the runtime being replaced.

use codex_core::CodexThread;
use codex_core::runtime_holds::RuntimeHold;
use codex_core::runtime_holds::RuntimeHolds;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;

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

    pub(super) fn bind(&mut self, runtime: &RuntimeHolds, viewed: bool) {
        if !matches!(&self.binding, RuntimeBinding::Bound { runtime: current, .. } if current.same_runtime(runtime))
        {
            self.binding = RuntimeBinding::Bound {
                runtime: runtime.clone(),
                viewer: None,
            };
        }
        if matches!(
            runtime.activity(),
            codex_core::runtime_holds::RuntimeActivity::Closing
        ) {
            return; // A created listener still drains an already-stopped runtime.
        }
        if !self.viewers(viewed) {
            return;
        }
        self.intents.retain(|intent| {
            if let Some(intent) = intent.upgrade() {
                let pending = matches!(
                    *intent
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    RuntimeUse::Pending
                );
                if pending {
                    ThreadRuntimeLease::bind_intent(&intent, runtime);
                }
                true
            } else {
                false
            }
        });
    }

    pub(super) fn viewers(&mut self, viewed: bool) -> bool {
        if let RuntimeBinding::Bound { runtime, viewer } = &mut self.binding {
            if !viewed {
                *viewer = None;
            } else if viewer.is_none() {
                let Some(hold) = runtime.acquire() else {
                    return false;
                };
                *viewer = Some(hold);
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
            let id = codex_protocol::ThreadId::from_string(input.thread_store.level_id())
                .expect("core thread extension data has a validated thread ID");
            let runtime = input
                .thread_store
                .get::<RuntimeHolds>()
                .expect("core installs runtime custody before lifecycle contributors");
            let mut state = self.0.state.lock().await;
            if let Some(entry) = state.threads.get_mut(&id) {
                entry
                    .runtime_leases
                    .bind(&runtime, !entry.connection_ids.is_empty());
            }
        })
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
        thread_id: codex_protocol::ThreadId,
    ) -> Result<super::SubscriberActivity, tokio::sync::TryLockError> {
        let state = self.state.try_lock()?;
        Ok(state.threads.get(&thread_id).map_or(
            super::SubscriberActivity::Detached {
                since: std::time::Instant::now(),
            },
            |entry| *entry.subscriber_activity.borrow(),
        ))
    }
}
