use std::sync::Mutex;
use std::sync::PoisonError;

use codex_code_mode::CellId;
use codex_code_mode::CodeModeNestedToolCall;
use codex_code_mode::CodeModeSessionDelegate;
use codex_code_mode::NotificationFuture;
use codex_code_mode::ToolInvocationFuture;
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct RecordingDelegate {
    pub(crate) invocations: Mutex<Vec<CodeModeNestedToolCall>>,
    pub(crate) notifications: Mutex<Vec<(String, CellId, String)>>,
    pub(crate) notification_delivered: Notify,
    pub(crate) closed_cells: Mutex<Vec<CellId>>,
    pub(crate) finished_cells: Mutex<Vec<(CellId, codex_code_mode::CellOutcome)>>,
    pub(crate) finished: Notify,
}

impl CodeModeSessionDelegate for RecordingDelegate {
    fn cell_finished(&self, cell_id: &CellId, outcome: codex_code_mode::CellOutcome) {
        self.finished_cells
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((cell_id.clone(), outcome));
        self.finished.notify_one();
    }
    fn invoke_tool<'a>(
        &'a self,
        invocation: CodeModeNestedToolCall,
        _cancellation: CancellationToken,
    ) -> ToolInvocationFuture<'a> {
        let native = invocation.tool_name == codex_protocol::ToolName::plain("exec_command");
        self.invocations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(invocation);
        Box::pin(async move {
            Ok(if native {
                json!({"wall_time_seconds":0.0,"exit_code":0,"output":"output"})
            } else {
                json!({ "value": "output" })
            })
        })
    }

    fn notify<'a>(
        &'a self,
        call_id: String,
        cell_id: CellId,
        text: String,
        _cancellation: CancellationToken,
    ) -> NotificationFuture<'a> {
        self.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((call_id, cell_id, text));
        self.notification_delivered.notify_one();
        Box::pin(async { Ok(()) })
    }

    fn cell_closed(&self, cell_id: &CellId) {
        self.closed_cells
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(cell_id.clone());
    }
}

pub(crate) fn cell_id(value: &str) -> CellId {
    CellId::new(value.to_string())
}
