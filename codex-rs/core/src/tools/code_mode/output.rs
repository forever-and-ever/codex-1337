//! Code Mode headers expose status; diagnostics retain host and handler timing.
//! Content is already truncated; only the bounded header changes at completion.

use std::time::Duration;

use codex_protocol::ExecutionOutputMetadata;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::function_call_output_content_items_to_text;
use codex_utils_audio::estimate_tool_audio_tokens;
use codex_utils_output_truncation::approx_token_count;

use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;

pub(super) struct CodeModeToolOutput {
    output: FunctionToolOutput,
    status: String,
    host_duration: Option<Duration>,
    timing_header: String,
    metadata: ExecutionOutputMetadata,
}

impl CodeModeToolOutput {
    pub(super) fn new(
        mut output: FunctionToolOutput,
        status: String,
        wall_time: Duration,
        host_duration: Option<Duration>,
        metadata: ExecutionOutputMetadata,
    ) -> Self {
        let wall_time_seconds = (wall_time.as_secs_f32() * 10.0).round() / 10.0;
        let timing_header =
            format!("{status}\nWall time {wall_time_seconds:.1} seconds\nOutput:\n");
        output.body.insert(
            /*index*/ 0,
            FunctionCallOutputContentItem::InputText {
                text: match metadata {
                    ExecutionOutputMetadata::Hidden => format!("{status}\nOutput:\n"),
                    ExecutionOutputMetadata::Included => timing_header.clone(),
                },
            },
        );
        Self {
            output,
            status,
            host_duration,
            timing_header,
            metadata,
        }
    }
}

impl codex_tools::HookToolOutput for CodeModeToolOutput {}

impl ToolOutput for CodeModeToolOutput {
    fn log_output(&self) -> String {
        format!(
            "{}{}",
            self.timing_header,
            function_call_output_content_items_to_text(&self.output.body[1..]).unwrap_or_default()
        )
    }

    fn success_for_logging(&self) -> bool {
        self.output.success_for_logging()
    }

    fn fallback_token_limit_override(&self) -> Option<usize> {
        // TELOS(recoverable-output): the Code Mode boundary already apportioned the model's text budget.
        // Preserve its sections, diagnostics and media through recording and replay:
        // a second prefix truncation would erase later sections and recovery paths.
        Some(
            self.output
                .body
                .iter()
                .map(|item| match item {
                    FunctionCallOutputContentItem::InputText { text } => approx_token_count(text),
                    FunctionCallOutputContentItem::InputAudio { audio } => {
                        estimate_tool_audio_tokens(audio)
                    }
                    FunctionCallOutputContentItem::InputImage { .. }
                    | FunctionCallOutputContentItem::EncryptedContent { .. } => 0,
                })
                .fold(0, usize::saturating_add),
        )
    }

    fn set_handler_duration_ms(&mut self, handler_duration_ms: u64) {
        let Some(host_duration) = self.host_duration else {
            return;
        };
        let total_seconds = handler_duration_ms as f64 / 1_000.0;
        let host_seconds = host_duration.as_secs_f64();
        // Match the logged measurements before rounding. Millisecond quantization of the
        // outer measurement can legitimately produce a small negative difference.
        let overhead_seconds = total_seconds - host_seconds;
        let status = &self.status;
        // The constructor owns this slot; script text and media stay untouched.
        self.timing_header = format!(
            "{status}\nWall time {total_seconds:.3} seconds (code-mode {host_seconds:.3} seconds; overhead {overhead_seconds:.3} seconds)\nOutput:\n"
        );
        if matches!(self.metadata, ExecutionOutputMetadata::Included) {
            self.output.body[0] = FunctionCallOutputContentItem::InputText {
                text: self.timing_header.clone(),
            };
        }
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        let mut item = self.output.to_response_item(call_id, payload);
        // Preserve the constructor's separate slot even with no script output;
        // flat notify strings must remain distinguishable from owned headers.
        if self.output.body.len() == 1 {
            match &mut item {
                ResponseInputItem::FunctionCallOutput { output, .. }
                | ResponseInputItem::CustomToolCallOutput { output, .. } => {
                    output.body = FunctionCallOutputBody::ContentItems(self.output.body.clone());
                }
                _ => unreachable!("Code Mode always produces a function or custom output"),
            }
        }
        item
    }
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
