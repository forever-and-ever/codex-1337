//! Bounded terminal outcomes outlive their process, never its stdin authority.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Weak;
use std::time::Duration;

use super::UnifiedExecContext;
use super::UnifiedExecError;
use super::UnifiedExecProcess;
use super::UnifiedExecProcessManager;
use super::WriteStdinRequest;
use super::format_output_omission_marker;
use crate::session::session::Session;
use crate::tools::context::ExecCommandToolOutput;
use codex_utils_output_truncation::approx_tokens_from_byte_count;

const MAX_RECEIPTS: usize = 64;
const MAX_TAIL_BYTES: usize = 16 * 1024;

#[derive(Default)]
pub(super) struct CompletionReceipts(VecDeque<(i32, CompletionReceipt)>);

struct CompletionReceipt {
    owner: Weak<Session>,
    call_id: String,
    chunk_id: String,
    exit_code: Option<i32>,
    tail: Vec<u8>,
    original_token_count: usize,
    omitted_bytes: Option<NonZeroUsize>,
}

impl CompletionReceipts {
    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    fn insert(&mut self, id: i32, receipt: CompletionReceipt) {
        self.0.retain(|(retained, _)| *retained != id);
        if self.0.len() == MAX_RECEIPTS {
            self.0.pop_front();
        }
        self.0.push_back((id, receipt));
    }

    fn poll(
        &self,
        context: &UnifiedExecContext,
        request: &WriteStdinRequest<'_>,
    ) -> Option<ExecCommandToolOutput> {
        if !request.input.is_empty() {
            return None;
        }
        let (_, receipt) = self.0.iter().find(|(id, receipt)| {
            *id == request.process_id && receipt.owner.ptr_eq(&Arc::downgrade(&context.session))
        })?;
        let mut output = receipt.omitted_bytes.map_or_else(Vec::new, |omitted| {
            format!("{}\n", format_output_omission_marker(omitted.get())).into_bytes()
        });
        output.extend_from_slice(&receipt.tail);
        Some(ExecCommandToolOutput {
            event_call_id: receipt.call_id.clone(),
            chunk_id: receipt.chunk_id.clone(),
            raw_output: output,
            process_id: None,
            exit_code: receipt.exit_code,
            wall_time: Duration::ZERO,
            original_token_count: Some(receipt.original_token_count),
            output_omitted_bytes: receipt.omitted_bytes,
            truncation_policy: request.truncation_policy,
            max_output_tokens: request.max_output_tokens,
            // Re-observation is not another command completion or hook execution.
            hook_context: None,
        })
    }
}

impl UnifiedExecProcessManager {
    pub(super) async fn completed_poll(
        &self,
        context: &UnifiedExecContext,
        request: &WriteStdinRequest<'_>,
    ) -> Result<ExecCommandToolOutput, UnifiedExecError> {
        self.process_store
            .lock()
            .await
            .completions
            .poll(context, request)
            .ok_or(UnifiedExecError::UnknownProcessId {
                process_id: request.process_id,
            })
    }

    pub(super) async fn retain_completion(
        &self,
        process_id: i32,
        process: &Arc<UnifiedExecProcess>,
        context: &UnifiedExecContext,
        response: &ExecCommandToolOutput,
    ) {
        if response.process_id.is_some() {
            return;
        }
        let receipt = {
            let buffer = process.output_handles().output_buffer.lock().await;
            let bytes = buffer.transcript.to_bytes();
            let tail = bytes[bytes.len().saturating_sub(MAX_TAIL_BYTES)..].to_vec();
            let omitted = buffer.transcript.total_bytes().saturating_sub(tail.len());
            CompletionReceipt {
                owner: Arc::downgrade(&context.session),
                call_id: response.event_call_id.clone(),
                chunk_id: response.chunk_id.clone(),
                exit_code: response.exit_code,
                tail,
                original_token_count: usize::try_from(approx_tokens_from_byte_count(
                    buffer.transcript.total_bytes(),
                ))
                .unwrap_or(usize::MAX),
                omitted_bytes: NonZeroUsize::new(omitted),
            }
        };
        let mut store = self.process_store.lock().await;
        if let Some(entry) = store.processes.get(&process_id)
            && !Arc::ptr_eq(&entry.process, process)
        {
            return;
        }
        store.processes.remove(&process_id);
        store.completions.insert(process_id, receipt);
    }
}
