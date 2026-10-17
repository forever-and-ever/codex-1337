use super::super::thread_lifecycle::log_listener_attach_result;
use super::super::token_usage_replay::send_thread_token_usage_update_to_connection;
use super::PAGINATED_FULL_HISTORY_DEPRECATION_SUMMARY;
use super::ThreadRequestProcessor;
use crate::outgoing_message::ConnectionRequestId;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadForkResponse;
use codex_core::CodexThread;
use codex_protocol::ThreadId;
use codex_protocol::protocol::W3cTraceContext;
use std::sync::Arc;

pub(super) fn snapshot_from_request(
    snapshot: codex_app_server_protocol::ThreadForkSnapshot,
) -> codex_core::ForkSnapshot {
    match snapshot {
        codex_app_server_protocol::ThreadForkSnapshot::Interrupted => {
            codex_core::ForkSnapshot::Interrupted
        }
        codex_app_server_protocol::ThreadForkSnapshot::Side => codex_core::ForkSnapshot::Side,
    }
}

pub(super) enum ThreadForkOrigin {
    Client(ConnectionRequestId),
    Coordination,
}

impl ThreadForkOrigin {
    pub(super) async fn warn_full_history(&self, processor: &ThreadRequestProcessor) {
        if let Self::Client(request_id) = self {
            processor
                .send_deprecation_notice(
                    request_id.connection_id,
                    PAGINATED_FULL_HISTORY_DEPRECATION_SUMMARY,
                )
                .await;
        }
    }

    pub(super) async fn request_trace(
        &self,
        processor: &ThreadRequestProcessor,
    ) -> Option<W3cTraceContext> {
        match self {
            Self::Client(request_id) => processor.request_trace_context(request_id).await,
            Self::Coordination => None,
        }
    }

    pub(super) async fn observe_runtime(
        &self,
        processor: &ThreadRequestProcessor,
        thread_id: ThreadId,
        thread: &Arc<CodexThread>,
    ) -> Result<(), JSONRPCErrorError> {
        match self {
            Self::Client(request_id) => {
                log_listener_attach_result(
                    processor
                        .ensure_conversation_listener(
                            thread_id,
                            request_id.connection_id,
                            /*raw_events_enabled*/ false,
                        )
                        .await,
                    thread_id,
                    request_id.connection_id,
                    "thread",
                );
                Ok(())
            }
            Self::Coordination => {
                let state = processor.thread_state_manager.thread_state(thread_id).await;
                processor
                    .ensure_listener_task_running(thread_id, Arc::clone(thread), state)
                    .await
            }
        }
    }

    pub(super) async fn publish_response(
        self,
        processor: &ThreadRequestProcessor,
        response: &ThreadForkResponse,
        thread_originator: String,
        thread_id: ThreadId,
        thread: &CodexThread,
        token_usage_turn_id: Option<String>,
    ) {
        if let Self::Client(request_id) = self {
            let connection_id = request_id.connection_id;
            processor
                .outgoing
                .send_response_with_thread_originator(
                    request_id,
                    response.clone(),
                    thread_originator,
                )
                .await;
            // `excludeTurns` is the cheap fork path, so skip restored usage replay
            // instead of rebuilding history only to attribute a historical update.
            if let Some(token_usage_turn_id) = token_usage_turn_id {
                // Mirror the resume contract for forks: the new thread is usable as soon
                // as the response arrives, so restored usage must follow immediately.
                send_thread_token_usage_update_to_connection(
                    &processor.outgoing,
                    connection_id,
                    thread_id,
                    thread,
                    token_usage_turn_id,
                )
                .await;
            }
        }
    }
}
