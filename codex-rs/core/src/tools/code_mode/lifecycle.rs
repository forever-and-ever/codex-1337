//! Execution lifetime and finite human-transcript attribution, independent of runtime cells.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::Weak;

use codex_code_mode::CellId;
use codex_protocol::code_mode::CodeModeEvent;
use codex_protocol::code_mode::CodeModeOwned;
use codex_protocol::code_mode::CodeModeOwner;
use codex_protocol::code_mode::CodeModeStatus;
use codex_protocol::protocol::EventMsg;
use tokio::sync::Notify;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::turn_timing::now_unix_timestamp_ms;

mod process_wait;
pub(crate) use process_wait::ProcessWait;

#[derive(Default)]
pub(super) struct TranscriptRegistry {
    state: Mutex<RegistryState>,
    tasks: TaskTracker,
    cleanup_started: std::sync::OnceLock<()>,
    cleanup_needed: Arc<Notify>,
}

#[derive(Default)]
struct RegistryState {
    active: HashMap<String, Arc<ExecutionScope>>,
    cells: HashMap<CellId, Weak<ExecutionScope>>,
    aliases: HashMap<String, Alias>,
}

struct Alias {
    owner: CodeModeOwner,
    scope: Weak<ExecutionScope>,
    anchor: CaptureAnchor,
    lifetime: AliasLifetime,
    pending_process: bool,
}

enum CaptureAnchor {
    Turn(Weak<TurnContext>),
    Queue(Weak<tokio::sync::Mutex<crate::state::TurnState>>),
}

impl CaptureAnchor {
    fn alive(&self) -> bool {
        match self {
            Self::Turn(turn) => turn.strong_count() != 0,
            Self::Queue(queue) => queue.strong_count() != 0,
        }
    }
}

enum AliasLifetime {
    Child,
    Capture {
        returned: bool,
        notifications: usize,
    },
}

/// Accepted notifications and returned batches fulfill distinct finite leases.
#[derive(Clone, Copy)]
pub(crate) enum CaptureOrigin {
    Returned,
    Notification,
}

pub(crate) struct NotificationLease {
    registry: Weak<TranscriptRegistry>,
    call: String,
    pending: bool,
}

impl NotificationLease {
    pub(crate) fn accepted(mut self) {
        self.pending = false;
    }
}

impl Drop for NotificationLease {
    fn drop(&mut self) {
        if self.pending
            && let Some(registry) = self.registry.upgrade()
        {
            registry.release_capture(&self.call, CaptureOrigin::Notification);
        }
    }
}

pub(super) struct ExecutionScope {
    owner: Mutex<CodeModeOwner>,
    session: Weak<Session>,
    turn: Arc<TurnContext>,
    registry: Weak<TranscriptRegistry>,
    admission: tokio::sync::Mutex<()>,
    shutdown: CancellationToken,
    terminal: Mutex<Option<TerminalRequest>>,
    changed: Notify,
    opened: watch::Sender<bool>,
    allocated: watch::Sender<bool>,
    admitted: watch::Sender<bool>,
}

#[derive(Clone, Copy)]
struct TerminalRequest {
    status: CodeModeStatus,
    at: i64,
    abandoned: bool,
}

pub(super) struct ExecutionStart(Option<Arc<ExecutionScope>>);

pub(super) struct ChildLease {
    scope: Arc<ExecutionScope>,
    call: String,
}

impl Drop for ChildLease {
    fn drop(&mut self) {
        if let Some(registry) = self.scope.registry.upgrade() {
            let mut state = registry
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if state.aliases.get(&self.call).is_some_and(|alias| {
                matches!(alias.lifetime, AliasLifetime::Child) && !alias.pending_process
            }) {
                state.aliases.remove(&self.call);
            }
            registry.cleanup_needed.notify_one();
        }
    }
}

impl ExecutionStart {
    pub(super) fn scope(&self) -> Arc<ExecutionScope> {
        Arc::clone(
            self.0
                .as_ref()
                .unwrap_or_else(|| panic!("execution start was transferred")),
        )
    }

    pub(super) async fn allocated(mut self, cell: &CellId) {
        let scope = self.scope();
        scope.allocate(cell);
        scope.record(&scope.turn, CodeModeEvent::Allocated).await;
        scope.allocated.send_replace(true);
        self.0.take();
    }

    pub(super) async fn failed(self) {
        let scope = self.scope();
        scope.finish(CodeModeStatus::Failed);
        scope.allocated.send_replace(true);
        scope.terminal_admitted().await;
    }
}

impl Drop for ExecutionStart {
    fn drop(&mut self) {
        if let Some(scope) = self.0.take() {
            scope.abandon();
            scope.allocated.send_replace(true);
        }
    }
}

impl TranscriptRegistry {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "the admission gate serializes transcript persistence/delivery; teardown cancels its drain"
    )]
    pub(super) async fn open(
        self: &Arc<Self>,
        session: Weak<Session>,
        turn: Arc<TurnContext>,
        exec_call_id: String,
        script: String,
        shutdown: CancellationToken,
    ) -> ExecutionStart {
        let draining_shutdown = shutdown.clone();
        self.cleanup_started.get_or_init(|| {
            let registry = Arc::downgrade(self);
            let cleanup_needed = Arc::clone(&self.cleanup_needed);
            self.tasks.spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = cleanup_needed.notified() => {}
                    }
                    loop {
                        let pending = {
                            let Some(registry) = registry.upgrade() else {
                                return;
                            };
                            let mut state = registry
                                .state
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner);
                            state.aliases.retain(|_, alias| alias.anchor.alive());
                            state.cells.retain(|_, scope| scope.strong_count() != 0);
                            state.aliases.values().any(|alias| {
                                !state.active.contains_key(&alias.owner.exec_call_id)
                                    || matches!(alias.anchor, CaptureAnchor::Queue(_))
                            })
                        };
                        if !pending {
                            break;
                        }
                        tokio::select! {
                            _ = shutdown.cancelled() => return,
                            _ = cleanup_needed.notified() => {},
                            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
                        }
                    }
                }
            });
        });
        let thread_id = session
            .upgrade()
            .map(|session| session.thread_id)
            .unwrap_or_else(|| panic!("execution requires its owning session"));
        let scope = Arc::new(ExecutionScope {
            owner: Mutex::new(CodeModeOwner {
                thread_id,
                exec_call_id: exec_call_id.clone(),
                cell_id: None,
                started_at_ms: now_unix_timestamp_ms(),
            }),
            session,
            turn,
            registry: Arc::downgrade(self),
            admission: tokio::sync::Mutex::new(()),
            shutdown: draining_shutdown.clone(),
            terminal: Mutex::new(None),
            changed: Notify::new(),
            opened: watch::channel(false).0,
            allocated: watch::channel(false).0,
            admitted: watch::channel(false).0,
        });
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .active
            .insert(exec_call_id.clone(), Arc::clone(&scope));
        self.alias(
            &scope,
            exec_call_id.clone(),
            &scope.turn,
            AliasLifetime::Capture {
                returned: false,
                notifications: 0,
            },
        );
        let starting = ExecutionStart(Some(Arc::clone(&scope)));
        let draining = Arc::clone(&scope);
        self.tasks.spawn(async move {
            let admission = async {
                draining
                    .record(
                        &draining.turn,
                        CodeModeEvent::Started {
                            call_id: exec_call_id,
                            script: Some(script),
                        },
                    )
                    .await;
                draining.opened.send_replace(true);
                let _ = draining
                    .allocated
                    .subscribe()
                    .wait_for(|allocated| *allocated)
                    .await;
                loop {
                    let changed = draining.changed.notified();
                    let terminal = *draining
                        .terminal
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    if terminal.is_some() {
                        break;
                    }
                    changed.await;
                }
                let _admission = draining.admission.lock().await;
                // An explicit outcome may supersede abandonment while admission
                // is blocked. Choose the outcome at its ordered admission point.
                let TerminalRequest { status, at, .. } = draining
                    .terminal
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .expect("terminal requested before admission");
                let owner = draining.owner();
                if let Some(session) = draining.session.upgrade() {
                    super::transcript::record(
                        &session,
                        &draining.turn,
                        owner,
                        CodeModeEvent::Finished { status },
                        at,
                    )
                    .await;
                }
            };
            // Service teardown already has a cancellation envelope. Cosmetic
            // persistence/delivery must not extend it while the event reader stops.
            tokio::select! {
                biased;
                _ = draining_shutdown.cancelled() => {},
                _ = admission => {},
            }
            if let Some(registry) = draining.registry.upgrade() {
                registry
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .active
                    .remove(&draining.owner().exec_call_id);
                registry.cleanup_needed.notify_one();
            }
            draining.opened.send_replace(true);
            draining.allocated.send_replace(true);
            draining.admitted.send_replace(true);
        });
        let _ = scope.opened.subscribe().wait_for(|opened| *opened).await;
        starting
    }

    fn alias(
        &self,
        scope: &Arc<ExecutionScope>,
        call_id: String,
        turn: &Arc<TurnContext>,
        lifetime: AliasLifetime,
    ) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .aliases
            .insert(
                call_id,
                Alias {
                    owner: scope.owner(),
                    scope: Arc::downgrade(scope),
                    anchor: CaptureAnchor::Turn(Arc::downgrade(turn)),
                    lifetime,
                    pending_process: false,
                },
            );
        self.cleanup_needed.notify_one();
    }

    pub(super) fn scope(&self, cell: &CellId) -> Option<Arc<ExecutionScope>> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cells
            .get(cell)
            .and_then(Weak::upgrade)
    }

    pub(super) fn owner(&self, call: &str) -> Option<CodeModeOwner> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(scope) = state.active.get(call) {
            return Some(scope.owner());
        }
        let alias = state.aliases.get(call)?;
        if !alias.anchor.alive() {
            return None;
        }
        Some(
            alias
                .scope
                .upgrade()
                .map_or_else(|| alias.owner.clone(), |scope| scope.owner()),
        )
    }

    pub(super) fn wrap(&self, event: EventMsg, owner: Option<CodeModeOwner>) -> EventMsg {
        assert!(
            !matches!(event, EventMsg::CodeModeOwned(_)),
            "nested Code Mode ownership is forbidden"
        );
        let Some(call) = event.code_mode_call_id() else {
            return event;
        };
        let owner = owner.or_else(|| self.owner(&call));
        match owner {
            Some(owner) => EventMsg::CodeModeOwned(CodeModeOwned::new(owner, Box::new(event))),
            None => event,
        }
    }

    pub(super) fn release_capture(&self, call: &str, origin: CaptureOrigin) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(alias) = state.aliases.get_mut(call)
            && let AliasLifetime::Capture {
                returned,
                notifications,
            } = &mut alias.lifetime
        {
            match origin {
                CaptureOrigin::Returned => *returned = true,
                CaptureOrigin::Notification => {
                    *notifications = notifications.checked_sub(1).unwrap_or_else(|| {
                        panic!("notification capture requires its admitted lease")
                    })
                }
            }
            if *returned && *notifications == 0 {
                state.aliases.remove(call);
            }
        }
        self.cleanup_needed.notify_one();
    }

    pub(super) fn release_child(&self, call: &str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .aliases
            .get(call)
            .is_some_and(|alias| matches!(alias.lifetime, AliasLifetime::Child))
        {
            state.aliases.remove(call);
        }
        self.cleanup_needed.notify_one();
    }

    pub(super) fn observe_child(&self, event: &EventMsg) {
        use codex_protocol::items::CommandExecutionStatus;
        use codex_protocol::items::TurnItem;
        let pending = match event.native_event() {
            EventMsg::ItemStarted(event) => {
                matches!(&event.item, TurnItem::CommandExecution(item) if item.status == CommandExecutionStatus::InProgress)
            }
            EventMsg::ItemCompleted(event) => {
                matches!(&event.item, TurnItem::CommandExecution(item) if item.status == CommandExecutionStatus::InProgress)
            }
            EventMsg::ExecCommandBegin(_) => true,
            _ => false,
        };
        if pending
            && let Some(call) = event.code_mode_call_id()
            && let Some(alias) = self
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .aliases
                .get_mut(call.as_ref())
        {
            alias.pending_process = true;
        }
    }

    pub(super) async fn shutdown(&self) {
        let scopes = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .active
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for scope in scopes {
            scope.finish(CodeModeStatus::Interrupted);
            scope.allocated.send_replace(true);
        }
        self.tasks.close();
        self.tasks.wait().await;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.active.clear();
        state.cells.clear();
        state.aliases.clear();
    }
}

impl ExecutionScope {
    pub(super) fn owner(&self) -> CodeModeOwner {
        self.owner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(super) fn allocate(self: &Arc<Self>, cell: &CellId) {
        let owner = {
            let mut owner = self.owner.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(allocated) = &owner.cell_id {
                assert_eq!(allocated, cell.as_str(), "conflicting runtime allocation");
            }
            owner.cell_id = Some(cell.to_string());
            owner.clone()
        };
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.cells.insert(cell.clone(), Arc::downgrade(self));
            for alias in state.aliases.values_mut() {
                if alias.owner.exec_call_id == owner.exec_call_id {
                    alias.owner = owner.clone();
                }
            }
        }
    }

    pub(super) fn finish(&self, status: CodeModeStatus) {
        let mut terminal = self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        if terminal.is_none_or(|terminal| terminal.abandoned) {
            *terminal = Some(TerminalRequest {
                status,
                at: now_unix_timestamp_ms(),
                abandoned: false,
            });
            self.changed.notify_one();
        }
    }

    pub(super) fn abandon(&self) {
        let mut terminal = self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        if terminal.is_none() {
            *terminal = Some(TerminalRequest {
                status: CodeModeStatus::Interrupted,
                at: now_unix_timestamp_ms(),
                abandoned: true,
            });
            self.changed.notify_one();
        }
    }

    pub(super) async fn terminal_admitted(&self) {
        let _ = self
            .admitted
            .subscribe()
            .wait_for(|admitted| *admitted)
            .await;
    }

    pub(super) async fn member(
        self: &Arc<Self>,
        call: String,
        turn: &Arc<TurnContext>,
    ) -> ChildLease {
        if let Some(registry) = self.registry.upgrade() {
            registry.alias(self, call.clone(), turn, AliasLifetime::Child);
        }
        let lease = ChildLease {
            scope: Arc::clone(self),
            call: call.clone(),
        };
        self.record(turn, CodeModeEvent::ToolCall { call_id: call })
            .await;
        lease
    }

    pub(super) fn capture_alias(self: &Arc<Self>, call: String, turn: &Arc<TurnContext>) {
        if let Some(registry) = self.registry.upgrade() {
            registry.alias(
                self,
                call,
                turn,
                AliasLifetime::Capture {
                    returned: false,
                    notifications: 0,
                },
            );
        }
    }
}

impl TranscriptRegistry {
    pub(super) fn notification(
        self: &Arc<Self>,
        owner: CodeModeOwner,
        call: String,
        queue: Weak<tokio::sync::Mutex<crate::state::TurnState>>,
    ) -> NotificationLease {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let scope = state
            .active
            .get(&owner.exec_call_id)
            .map(Arc::downgrade)
            .or_else(|| {
                owner
                    .cell_id
                    .as_ref()
                    .and_then(|cell| state.cells.get(&CellId::new(cell.clone())).cloned())
            })
            .unwrap_or_default();
        let alias = state.aliases.entry(call.clone()).or_insert_with(|| Alias {
            owner: owner.clone(),
            scope,
            anchor: CaptureAnchor::Queue(queue.clone()),
            // The original returned lease was already fulfilled if this alias
            // did not exist. This notification owns only its accepted capture.
            lifetime: AliasLifetime::Capture {
                returned: true,
                notifications: 0,
            },
            pending_process: false,
        });
        assert_eq!(
            alias.owner.exec_call_id, owner.exec_call_id,
            "conflicting capture ownership"
        );
        let AliasLifetime::Capture { notifications, .. } = &mut alias.lifetime else {
            panic!("notification cannot borrow a child alias")
        };
        *notifications += 1;
        alias.anchor = CaptureAnchor::Queue(queue);
        self.cleanup_needed.notify_one();
        NotificationLease {
            registry: Arc::downgrade(self),
            call,
            pending: true,
        }
    }
}

impl ExecutionScope {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "the admission gate preserves scope event order across persistence/delivery"
    )]
    pub(super) async fn record(self: &Arc<Self>, turn: &TurnContext, event: CodeModeEvent) {
        let admission = async {
            let _admission = self.admission.lock().await;
            let owner = self.owner();
            if let Some(session) = self.session.upgrade() {
                super::transcript::record(&session, turn, owner, event, now_unix_timestamp_ms())
                    .await;
            }
        };
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => {},
            _ = admission => {},
        }
    }
}

impl Drop for ExecutionScope {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.cleanup_needed.notify_one();
        }
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
