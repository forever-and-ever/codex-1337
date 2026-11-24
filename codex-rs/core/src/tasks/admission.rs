//! Reserves the exact pending turn and acquires host capacity without blocking dispatch.

use crate::agent::host::AgentTurnAdmission;
use crate::agent::host::AgentTurnAdmissionRequest;
use crate::hook_environment_executor::with_environment_hooks;
use crate::hook_runtime::record_pending_input;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn::recordable_inputs;
use crate::session::turn::run_hooks_and_record_inputs;
use crate::session::turn_context::TurnContext;
use crate::state::ActiveTurn;
use crate::state::TaskKind;
use crate::state::TurnState;
use codex_async_utils::OrCancelExt;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::TurnAbortReason;
use codex_thread_store::PersistContext;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;
use tokio_util::sync::CancellationToken;

// TELOS(server-hosted-agents): pending-work reservations cannot recreate replaced turns.
pub(crate) enum TaskAdmission {
    Unreserved(Vec<TurnInput>),
    Reserved {
        input: Vec<TurnInput>,
        turn_state: Arc<Mutex<TurnState>>,
    },
}

impl From<Vec<TurnInput>> for TaskAdmission {
    fn from(input: Vec<TurnInput>) -> Self {
        Self::Unreserved(input)
    }
}

impl TaskAdmission {
    pub(super) fn matches(&self, turn: &ActiveTurn) -> bool {
        match self {
            Self::Unreserved(_) => true,
            Self::Reserved { turn_state, .. } => Arc::ptr_eq(&turn.turn_state, turn_state),
        }
    }

    pub(super) async fn lock<'a>(
        &self,
        session: &'a Session,
    ) -> Option<MutexGuard<'a, Option<ActiveTurn>>> {
        let mut active = session.active_turn.lock().await;
        if session.runtime_holds.is_closing() {
            return None;
        }
        match self {
            Self::Unreserved(_) => {
                if active.is_none() {
                    *active = Some(session.reserve_runtime_turn()?);
                }
            }
            Self::Reserved { .. } => {
                if active
                    .as_ref()
                    .is_none_or(|turn| !self.matches(turn) || turn.task.is_some())
                {
                    return None;
                }
            }
        }
        Some(active)
    }

    pub(super) fn into_input(self) -> Vec<TurnInput> {
        match self {
            Self::Unreserved(input) | Self::Reserved { input, .. } => input,
        }
    }
}

/// The host admission path already emitted the logical turn's start event.
pub(crate) struct HostTurnStarted;

pub(super) async fn admit(
    session: &Arc<Session>,
    context: &Arc<TurnContext>,
    task_kind: TaskKind,
    provider: &dyn AgentTurnAdmission,
    cancellation: &CancellationToken,
) -> Result<Vec<TurnInput>> {
    // Review forwards the delegate's start event. Other tasks announce the attempt
    // here so an unresolved admission remains visible and interruptible.
    if task_kind != TaskKind::Review {
        session.emit_turn_started(context, task_kind).await;
        context.extension_data.insert(HostTurnStarted);
    }
    let guard = provider
        .admit_turn(AgentTurnAdmissionRequest {
            thread_id: session.thread_id,
            turn_id: context.sub_id.clone(),
        })
        .or_cancel(cancellation)
        .await
        .map_err(|_| CodexErr::new(CodexErrorDetails::TurnAborted))??;

    // Acquiring this lock waits for turn registration to finish before attaching the permit.
    // A cancelled/replaced task must never install its permit into a newer task,
    // even if recovery reused the turn ID.
    let mut active = session.active_turn.lock().await;
    let task = active
        .as_mut()
        .and_then(|turn| turn.task.as_mut())
        .filter(|task| Arc::ptr_eq(&task.turn_context, context))
        .filter(|_| !cancellation.is_cancelled())
        .ok_or_else(|| CodexErr::new(CodexErrorDetails::TurnAborted))?;
    task._agent_execution_guard = Some(guard);
    task.pending_initial_input.take().ok_or_else(|| {
        CodexErr::Fatal("initial input missing from the task awaiting host admission".into())
    })
}

impl Session {
    pub(crate) fn host_turn_admission(&self) -> Option<Arc<dyn AgentTurnAdmission>> {
        match &self.agent_runtime {
            crate::agent::host::AgentRuntimeServices::Controller => None,
            crate::agent::host::AgentRuntimeServices::Host(services) => {
                Some(Arc::clone(&services.turn_admission))
            }
        }
    }

    /// Interruption and suspension stop the task before it can persist its initial input.
    pub(crate) async fn record_pending_initial_input(
        self: &Arc<Self>,
        context: &Arc<TurnContext>,
        turn_state: &Mutex<TurnState>,
        input: Option<Vec<TurnInput>>,
        abort_reason: Option<&TurnAbortReason>,
    ) -> Result<()> {
        let Some(mut input) = input else {
            return Ok(());
        };
        input.extend(
            self.input_queue
                .take_pending_input_for_turn_state(turn_state)
                .await,
        );
        let model_info = context.capture_current_model_info();
        if abort_reason != Some(&TurnAbortReason::Interrupted) {
            run_hooks_and_record_inputs(
                self,
                context,
                &model_info,
                &input,
                PersistContext::Standard,
            )
            .await?;
            return Ok(());
        }

        let mut input_error = None;
        for item in recordable_inputs(self, &model_info, &input) {
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    input_error.get_or_insert(error);
                    continue;
                }
            };
            // The user must resubmit a prompt that still needs a submit hook.
            if matches!(item.as_ref(), TurnInput::UserInput { .. })
                && with_environment_hooks(
                    self,
                    context,
                    &context.initial_environments.refresh_readiness(),
                )
                .execution_mode_for_event(HookEventName::UserPromptSubmit)
                .is_some()
            {
                if let TurnInput::UserInput { metadata, .. } = item.as_ref()
                    && let Some(uuid) = metadata.realtime_handoff_id
                {
                    self.reject_realtime_handoff(uuid).await;
                }
                continue;
            }
            if let Err(error) = record_pending_input(
                self,
                context,
                &model_info,
                item.into_owned(),
                Vec::new(),
                PersistContext::Standard,
            )
            .await
            {
                input_error.get_or_insert(error);
            }
        }
        if let Some(error) = input_error {
            self.conversation.stop_for_history_recovery().await;
            return Err(error);
        }
        Ok(())
    }
}
