//! Urgent control bypasses admission barriers without abandoning request ownership.

use super::Arc;
use super::ClientRequest;
use super::ExitReason;
use super::Mutex;
use super::OperationReceipt;
use super::Result;
use super::ThreadEventStore;
use super::ThreadId;
use super::TurnStatus;
use super::TypedRequestError;
use super::Uuid;
use super::active_turn_interrupt_race;
use super::detach;
use super::event_dispatch;
use crate::app_server_session::typed_requests::SessionRpc;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

#[derive(Clone)]
pub(in crate::app) enum ControlOperation {
    InterruptActive {
        paginated_history: bool,
    },
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
    RealtimeStop {
        attempt_id: u64,
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
        ControlOperation::RealtimeStop { .. } => {
            let _: api::ThreadRealtimeStopResponse = handle
                .request_typed(ClientRequest::ThreadRealtimeStop {
                    request_id: api::RequestId::String(format!("tui-operation-{id}-voice-stop")),
                    params: api::ThreadRealtimeStopParams {
                        thread_id: thread_id.to_string(),
                    },
                })
                .await?;
            return Ok(OperationReceipt::RealtimeStopped);
        }
        ControlOperation::DetachAndExit { plan, .. } => {
            let receipt = match tokio::time::timeout(
                event_dispatch::SHUTDOWN_FIRST_EXIT_TIMEOUT,
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
        ControlOperation::InterruptActive { paginated_history } => {
            let cached = store.lock().await.active_turn_id().map(str::to_owned);
            let turn_id = if let Some(turn_id) = cached {
                Some(turn_id)
            } else {
                let thread: api::ThreadReadResponse = handle
                    .thread_read(
                        api::RequestId::String(format!("tui-operation-{id}-read")),
                        api::ThreadReadParams {
                            thread_id: thread_id.to_string(),
                            include_turns: false,
                        },
                    )
                    .await?;
                let history = if thread.thread.history_mode == api::ThreadHistoryMode::Paginated
                    && paginated_history
                {
                    None
                } else {
                    match handle
                        .thread_read(
                            api::RequestId::String(format!("tui-operation-{id}-history")),
                            api::ThreadReadParams {
                                thread_id: thread_id.to_string(),
                                include_turns: true,
                            },
                        )
                        .await
                    {
                        Ok(history) => Some(history.thread.turns),
                        // Legacy negotiation can coexist with a migrated paginated rollout.
                        Err(TypedRequestError::Server { ref source, .. })
                            if source.message
                                == "paginated threads do not support thread/read(includeTurns=true)" =>
                        {
                            None
                        }
                        Err(error) => return Err(error),
                    }
                };
                let turns = if let Some(turns) = history {
                    turns
                } else {
                    let page: api::ThreadTurnsListResponse = handle
                        .request_typed(ClientRequest::ThreadTurnsList {
                            request_id: api::RequestId::String(format!("tui-operation-{id}-turns")),
                            params: api::ThreadTurnsListParams {
                                thread_id: thread_id.to_string(),
                                cursor: None,
                                limit: Some(crate::app_server_session::INITIAL_HISTORY_TURN_LIMIT),
                                sort_direction: Some(api::SortDirection::Desc),
                                items_view: Some(api::TurnItemsView::NotLoaded),
                            },
                        })
                        .await?;
                    page.data
                };
                turns
                    .into_iter()
                    .find(|turn| turn.status == TurnStatus::InProgress)
                    .map(|turn| turn.id)
            };
            store.lock().await.pending_interrupt_turn_id = turn_id.clone();
            (turn_id, false)
        }
        ControlOperation::StopAndExit { turn_id, goal } => {
            if matches!(goal, GoalStop::Pause) {
                let _: api::ThreadGoalSetResponse = handle
                    .thread_goal_set(
                        api::RequestId::String(format!("tui-operation-{id}-pause")),
                        api::ThreadGoalSetParams {
                            thread_id: thread_id.to_string(),
                            origin: Some(api::ThreadGoalMutationOrigin::User),
                            objective: None,
                            status: Some(api::ThreadGoalStatus::Paused),
                            token_budget: None,
                        },
                    )
                    .await?;
            }
            (turn_id, true)
        }
    };
    if let Some(mut turn_id) = turn_id {
        for attempt in 0..2 {
            let result = handle
                .turn_interrupt(
                    api::RequestId::String(format!("tui-operation-{id}-interrupt-{attempt}")),
                    api::TurnInterruptParams {
                        thread_id: thread_id.to_string(),
                        turn_id: turn_id.clone(),
                    },
                )
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
