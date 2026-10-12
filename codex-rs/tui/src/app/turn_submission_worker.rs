//! Await submission acknowledgements off the App loop, without holding a thread-store lock.

use super::*;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::RequestId as AppServerRequestId;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnSteerCancelParams;
use codex_app_server_protocol::TurnSteerCancelResponse;
use codex_app_server_protocol::TurnSteerParams;
use codex_app_server_protocol::TurnSteerReplaceParams;
use codex_app_server_protocol::TurnSteerReplaceResponse;
use codex_app_server_protocol::TurnSteerResponse;

// These RPCs acknowledge acceptance, not turn completion. A missing acknowledgement
// after the same bound used for farm receipts is uncertain, never permission to retry.
pub(super) const ACK_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 30);

fn request_id() -> AppServerRequestId {
    AppServerRequestId::String(format!("submit-{}", uuid::Uuid::new_v4()))
}

pub(super) async fn send(
    handle: AppServerRequestHandle,
    request: WorkerRequest,
    origin: OperationOrigin,
) -> SubmissionOutcome {
    if let WorkerRequest::Operation { id, operation } = request {
        let result = if matches!(
            &*operation,
            ServerOperation::Mutation(requests::ThreadMutation::ServerShell(_))
        ) {
            requests::send(handle, id, *operation, origin)
                .await
                .map_err(classify_failure)
        } else {
            match tokio::time::timeout(ACK_TIMEOUT, requests::send(handle, id, *operation, origin))
                .await
            {
                Ok(result) => result.map_err(classify_failure),
                Err(_) => Err(SubmissionFailure::TimedOut),
            }
        };
        return SubmissionOutcome::Operation(result);
    }
    let (thread_id, store) = origin.thread();
    match tokio::time::timeout(
        ACK_TIMEOUT,
        send_inner(handle, thread_id, request, Arc::clone(store)),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => SubmissionOutcome::Finished {
            replacement: ReplacementContinuation::NoReceipt,
            observations: Vec::new(),
            result: Err(SubmissionFailure::TimedOut),
        },
    }
}

async fn send_inner(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    request: WorkerRequest,
    store: Arc<Mutex<ThreadEventStore>>,
) -> SubmissionOutcome {
    let (params, replacement, expected_turn_id) = match request {
        WorkerRequest::Operation { .. } => unreachable!("RPC request handled before turn dispatch"),
        WorkerRequest::Continuation { params, review } => {
            let eligible = {
                let store = store.lock().await;
                super::super::misalignment_policy::review_matches_store(&store, &review)
            };
            if !eligible {
                return finished(ReplacementContinuation::NoReceipt, Vec::new(), Ok(SubmissionReceipt::Deferred("This conversation changed while continuation was queued. No continuation was sent.".into())));
            }
            let result = handle
                .request_typed::<TurnStartResponse>(ClientRequest::TurnStart {
                    request_id: request_id(),
                    params: *params,
                })
                .await;
            return finished(
                ReplacementContinuation::NoReceipt,
                Vec::new(),
                result.map(|response| SubmissionReceipt::Continued(response.turn)),
            );
        }
        WorkerRequest::Cancel { client_id } => {
            let result: Result<TurnSteerCancelResponse, TypedRequestError> = handle
                .request_typed(ClientRequest::TurnSteerCancel {
                    request_id: request_id(),
                    params: TurnSteerCancelParams {
                        thread_id: thread_id.to_string(),
                        client_user_message_id: client_id,
                    },
                })
                .await;
            return finished(
                ReplacementContinuation::NoReceipt,
                Vec::new(),
                result.map(|response| SubmissionReceipt::Cancelled(response.cancelled)),
            );
        }
        WorkerRequest::Turn {
            command: WorkerCommand::User { replacement },
            params,
        } => (params, replacement, None),
        WorkerRequest::Turn {
            command: WorkerCommand::Subagent { expected_turn_id },
            params,
        } => (params, None, expected_turn_id),
    };
    let mut continuation = ReplacementContinuation::NoReceipt;
    if let Some(previous) = replacement {
        let response: Result<TurnSteerReplaceResponse, TypedRequestError> = handle
            .request_typed(ClientRequest::TurnSteerReplace {
                request_id: request_id(),
                params: TurnSteerReplaceParams {
                    thread_id: thread_id.to_string(),
                    client_user_message_id: previous.client_user_message_id,
                    input: previous.input,
                },
            })
            .await;
        match response {
            Ok(response) if response.replaced => return SubmissionOutcome::Replaced,
            Ok(_) => continuation = ReplacementContinuation::Missed,
            Err(error) => return finished(continuation, Vec::new(), Err(error)),
        }
    }
    let mut observations = Vec::new();
    let result: Result<SubmissionReceipt, TypedRequestError> = async {
        let active_turn = match expected_turn_id {
            Some(id) => Some(id),
            None => store.lock().await.active_turn_id().map(str::to_owned),
        };
        if let Some(mut turn_id) = active_turn {
            for attempt in 0..2 {
                let response: Result<TurnSteerResponse, TypedRequestError> = handle
                    .request_typed(ClientRequest::TurnSteer {
                        request_id: request_id(),
                        params: TurnSteerParams {
                            thread_id: thread_id.to_string(),
                            client_user_message_id: params.client_user_message_id.clone(),
                            input: params.input.clone(),
                            responsesapi_client_metadata: None,
                            additional_context: None,
                            expected_turn_id: turn_id.clone(),
                        },
                    })
                    .await;
                match response {
                    Ok(_) => return Ok(SubmissionReceipt::Steered),
                    Err(error) => {
                        if let Some(error) = active_turn_not_steerable_turn_error(&error) {
                            return Ok(SubmissionReceipt::Deferred(error.message));
                        }
                        match active_turn_steer_race(&error) {
                            Some(ActiveTurnSteerRace::Missing) => {
                                observations.push(TurnObservation::Missing { expected: turn_id });
                                break;
                            }
                            Some(ActiveTurnSteerRace::ExpectedTurnMismatch { actual_turn_id }) => {
                                observations.push(TurnObservation::Mismatch {
                                    expected: turn_id.clone(),
                                    actual: actual_turn_id.clone(),
                                });
                                if attempt == 0 && actual_turn_id != turn_id {
                                    turn_id = actual_turn_id;
                                } else {
                                    return Err(error);
                                }
                            }
                            None => return Err(error),
                        }
                    }
                }
            }
        }
        let previous_latest = store.lock().await.latest_turn_id.clone();
        let response: TurnStartResponse = handle
            .request_typed(ClientRequest::TurnStart {
                request_id: request_id(),
                params: *params,
            })
            .await?;
        Ok(SubmissionReceipt::Started {
            turn_id: response.turn.id,
            previous_latest,
        })
    }
    .await;
    finished(continuation, observations, result)
}
fn classify_failure(error: TypedRequestError) -> SubmissionFailure {
    match error {
        TypedRequestError::Server { .. } => SubmissionFailure::Rejected(error),
        TypedRequestError::Transport { .. } | TypedRequestError::Deserialize { .. } => {
            SubmissionFailure::Uncertain(error)
        }
    }
}
fn finished(
    replacement: ReplacementContinuation,
    observations: Vec<TurnObservation>,
    result: Result<SubmissionReceipt, TypedRequestError>,
) -> SubmissionOutcome {
    SubmissionOutcome::Finished {
        replacement,
        observations,
        result: result.map_err(classify_failure),
    }
}
