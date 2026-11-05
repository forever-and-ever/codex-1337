//! One-shot execution over the shared unified-exec lifecycle.
//!
//! The caller never receives a resumable process. Timeout and cancellation
//! terminate the exact process handle published by the normal startup path.

use std::sync::Arc;
use std::sync::OnceLock;

use tokio::time::Duration;
use tokio::time::Instant;
use tracing::Instrument;

use super::ExecCommandRequest;
use super::UnifiedExecContext;
use super::UnifiedExecError;
use super::UnifiedExecProcess;
use super::UnifiedExecProcessManager;
use super::trace_id;
use crate::tools::context::ExecCommandToolOutput;

pub(super) struct Completion<'a> {
    pub process_ready: &'a tokio::sync::Notify,
    pub timeout: Duration,
    pub timed_out: bool,
    pub process: &'a OnceLock<Arc<UnifiedExecProcess>>,
}

impl UnifiedExecProcessManager {
    #[tracing::instrument(
        name = "unified_exec.exec_command",
        level = "info",
        skip_all,
        fields(
            conversation.id = %context.session.thread_id,
            turn_id = trace_id(&context.step_context.turn.sub_id),
            call_id = trace_id(&context.call_id),
            unified_exec_process_id = request.process_id,
            mode = "oneshot",
            outcome = tracing::field::Empty,
        )
    )]
    pub(crate) async fn exec_command_to_completion(
        request: ExecCommandRequest,
        context: &UnifiedExecContext,
        timeout: Duration,
    ) -> Result<ExecCommandToolOutput, UnifiedExecError> {
        let context = UnifiedExecContext::new(
            Arc::clone(&context.session),
            Arc::clone(&context.step_context),
            context.cancellation_token.child_token(),
            context.call_id.clone(),
            context.tool_name.clone(),
        );
        let _cancel_on_drop = context.cancellation_token.clone().drop_guard();
        let lifetimes = Arc::clone(&context.session.services.unified_exec_manager.lifetimes);
        let runtime = context.session.runtime_holds.clone();
        let process_id = request.process_id;
        let task = async move {
            let manager = &context.session.services.unified_exec_manager;
            let process_id = request.process_id;
            if Instant::now().checked_add(timeout).is_none() {
                manager.release_process_id(process_id).await;
                return (
                    Err(UnifiedExecError::process_failed(
                        "timeout_ms is too large".into(),
                    )),
                    "failed",
                );
            }

            let process = OnceLock::new();
            let process_ready = tokio::sync::Notify::new();
            let mut completion = Completion {
                process_ready: &process_ready,
                timeout,
                timed_out: false,
                process: &process,
            };
            let result = {
                let mut execution =
                    Box::pin(manager.exec_command_inner(request, &context, Some(&mut completion)));
                let cancellation = async {
                    context.cancellation_token.cancelled().await;
                    loop {
                        let ready = process_ready.notified();
                        if let Some(process) = process.get() {
                            if let Err(error) = process.terminate_confirmed().await {
                                process.fail_and_terminate(error.to_string());
                            }
                            process.cancellation_token().cancel();
                            return;
                        }
                        ready.await;
                    }
                };
                tokio::pin!(cancellation);
                tokio::select! {
                    biased;
                    () = &mut cancellation => {
                        let _ = execution.await;
                        return (
                            Err(UnifiedExecError::process_failed("command cancelled".into())),
                            "cancelled",
                        );
                    },
                    result = &mut execution => result,
                }
            };

            let outcome = if completion.timed_out {
                "timed_out"
            } else if result.is_ok() {
                "exited"
            } else {
                "failed"
            };
            let result = result.map(|mut output| {
                if completion.timed_out {
                    output.process_id = None;
                }
                output
            });
            (result, outcome)
        };
        lifetimes
            .execute(&runtime, process_id, move || async move {
                let (result, outcome) = task.in_current_span().await;
                tracing::Span::current().record("outcome", outcome);
                result
            })
            .await
    }
}
