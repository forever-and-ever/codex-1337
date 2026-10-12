//! Urgent control bypasses admission barriers without abandoning request ownership.

use super::*;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

#[derive(Clone)]
pub(in crate::app) enum ControlOperation {
    Interrupt {
        turn_id: String,
    },
    DetachAndExit {
        plan: detach::DetachPlan,
        reason: ExitReason,
    },
    StopAndExit {
        turn_id: Option<String>,
        goal: GoalStop,
    },
}

#[derive(Clone, Copy)]
pub(in crate::app) enum GoalStop {
    Preserve,
    Pause,
}

pub(super) async fn send(
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
    id: Uuid,
    control: ControlOperation,
    store: &Arc<Mutex<ThreadEventStore>>,
) -> Result<OperationReceipt, TypedRequestError> {
    let (turn_id, exit) = match control {
        ControlOperation::DetachAndExit { plan, .. } => {
            let receipt = match tokio::time::timeout(
                super::super::event_dispatch::SHUTDOWN_FIRST_EXIT_TIMEOUT,
                detach::send(handle, id, &plan),
            )
            .await
            {
                Ok(results) => detach::DetachReceipt::Complete(results),
                Err(_) => detach::DetachReceipt::BudgetExpired,
            };
            return Ok(OperationReceipt::ExitDetached(receipt));
        }
        ControlOperation::Interrupt { turn_id } => (Some(turn_id), false),
        ControlOperation::StopAndExit { turn_id, goal } => {
            if matches!(goal, GoalStop::Pause) {
                let _: api::ThreadGoalSetResponse = handle
                    .request_typed(api::ClientRequest::ThreadGoalSet {
                        request_id: api::RequestId::String(format!("tui-operation-{id}-pause")),
                        params: api::ThreadGoalSetParams {
                            thread_id: thread_id.to_string(),
                            origin: Some(api::ThreadGoalMutationOrigin::User),
                            objective: None,
                            status: Some(api::ThreadGoalStatus::Paused),
                            token_budget: None,
                        },
                    })
                    .await?;
            }
            (turn_id, true)
        }
    };
    if let Some(mut turn_id) = turn_id {
        for attempt in 0..2 {
            let result = handle
                .request_typed::<api::TurnInterruptResponse>(api::ClientRequest::TurnInterrupt {
                    request_id: api::RequestId::String(format!(
                        "tui-operation-{id}-interrupt-{attempt}"
                    )),
                    params: api::TurnInterruptParams {
                        thread_id: thread_id.to_string(),
                        turn_id: turn_id.clone(),
                    },
                })
                .await;
            match result {
                Ok(_) => break,
                Err(error) => {
                    if attempt == 0
                        && !exit
                        && let Some(actual) = active_turn_interrupt_race(&error)
                        && actual != turn_id
                    {
                        store.lock().await.pending_interrupt_turn_id = Some(actual.clone());
                        turn_id = actual;
                        continue;
                    }
                    if exit && store.lock().await.active_turn_id().is_none() {
                        break;
                    }
                    return Err(error);
                }
            }
        }
    }
    Ok(if exit {
        OperationReceipt::StopAndExitCompleted
    } else {
        OperationReceipt::Interrupted
    })
}
