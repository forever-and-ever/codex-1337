//! Farm is a root-thread RPC, never a queued turn or steer. Refuse new farms and UI
//! replacement while a request awaits its receipt or its outcome is unknown; loss of a
//! response is not a failed spawn. A rejection spawned nothing, so it retains nothing.

use super::transcript_splice::SpliceShape;
use super::transcript_splice::TranscriptSplice;
use super::*;
use crate::chatwidget::FarmSubmission;
use codex_app_server_protocol::ThreadFarmMode;
use codex_app_server_protocol::ThreadFarmParams;
use codex_app_server_protocol::ThreadFarmResponse;

const MAX_FARM_RECEIPTS: usize = 64;

#[derive(Debug)]
pub(crate) enum FarmFailure {
    Rejected(String),
    Uncertain(String),
}

#[derive(Debug, Clone)]
pub(super) struct RetainedFarm {
    thread_id: ThreadId,
    request_id: String,
    mode: ThreadFarmMode,
    submission: FarmSubmission,
}

#[derive(Default)]
pub(super) enum FarmRequestState {
    #[default]
    Idle,
    Waiting(RetainedFarm),
    Uncertain(RetainedFarm),
}

impl FarmRequestState {
    fn retained(&self) -> Option<&RetainedFarm> {
        match self {
            Self::Waiting(retained) | Self::Uncertain(retained) => Some(retained),
            Self::Idle => None,
        }
    }
}

#[derive(Default)]
pub(super) struct FarmUi {
    pub(super) state: FarmRequestState,
    receipts: VecDeque<FarmReceipt>,
}

#[derive(Debug, Clone)]
struct FarmReceipt {
    thread_id: ThreadId,
    request_id: String,
    message: String,
}

impl HistoryCell for FarmReceipt {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        history_cell::new_info_event(self.message.clone(), /*hint*/ None).display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![self.message.clone().into()]
    }
}

impl App {
    pub(super) async fn start_farm(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        mode: ThreadFarmMode,
        submission: FarmSubmission,
    ) {
        if self.primary_thread_id != Some(thread_id)
            || self.current_displayed_thread_id() != Some(thread_id)
            || self.chat_widget.is_parent_owned_thread()
        {
            self.chat_widget
                .add_error_message("Return to the main conversation before farming a task.".into());
            return;
        }
        match &self.farm.state {
            FarmRequestState::Uncertain(pending) => {
                self.chat_widget.add_error_message(format!("Farm outcome is unknown; use /farm to check or discard. Request {} is retained.", pending.request_id));
                return;
            }
            FarmRequestState::Waiting(_) => {
                self.chat_widget
                    .add_error_message("A farm request is already waiting for its receipt.".into());
                return;
            }
            FarmRequestState::Idle => {}
        }
        let request_id = Uuid::new_v4().to_string();
        self.farm.state = FarmRequestState::Waiting(RetainedFarm {
            thread_id,
            request_id,
            mode,
            submission,
        });
        self.send_pending_farm(tui, server).await;
    }

    async fn send_pending_farm(&mut self, tui: &mut tui::Tui, server: &mut AppServerSession) {
        let pending = match std::mem::take(&mut self.farm.state) {
            FarmRequestState::Waiting(pending) | FarmRequestState::Uncertain(pending) => pending,
            FarmRequestState::Idle => return,
        };
        let request_id = pending.request_id.clone();
        let origin = pending.thread_id;
        let params = ThreadFarmParams {
            thread_id: pending.thread_id.to_string(),
            request_id: request_id.clone(),
            mode: pending.mode,
            input: pending.submission.input.clone(),
            task_name: pending.submission.task_name.clone(),
        };
        self.farm.state = FarmRequestState::Waiting(pending);
        self.set_farm_receipt(tui, origin, &request_id, "Farming task…".into())
            .await;
        let handle = server.request_handle();
        let rpc_id = server.next_request_id();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(/*secs*/ 30),
                handle.request_typed::<ThreadFarmResponse>(ClientRequest::ThreadFarm {
                    request_id: rpc_id,
                    params,
                }),
            )
            .await
            {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(TypedRequestError::Server { source, .. })) if source.code == -32601 => {
                    Err(FarmFailure::Rejected(
                        "Connected server does not support /farm yet; server upgrade required"
                            .into(),
                    ))
                }
                Ok(Err(TypedRequestError::Server { source, .. }))
                    if source.data.as_ref().is_some_and(|data| {
                        data.get("farmOutcome").and_then(serde_json::Value::as_str)
                            == Some("rejected")
                            && data.get("requestId").and_then(serde_json::Value::as_str)
                                == Some(request_id.as_str())
                    }) =>
                {
                    Err(FarmFailure::Rejected(source.message))
                }
                Ok(Err(error)) => Err(FarmFailure::Uncertain(error.to_string())),
                Err(_) => Err(FarmFailure::Uncertain("receipt timed out".into())),
            };
            tx.send(AppEvent::FarmFinished { request_id, result });
        });
    }

    pub(super) fn open_farm_recovery(&mut self, thread_id: ThreadId) {
        if self.primary_thread_id != Some(thread_id)
            || self.current_displayed_thread_id() != Some(thread_id)
        {
            return;
        }
        match &self.farm.state {
            FarmRequestState::Uncertain(pending) => {
                self.chat_widget
                    .open_farm_recovery(pending.request_id.clone());
            }
            FarmRequestState::Waiting(_) => self
                .chat_widget
                .add_error_message("A farm request is still waiting for its receipt.".into()),
            FarmRequestState::Idle => self
                .chat_widget
                .add_error_message("Usage: /farm <task> or /farm_fresh <task>".into()),
        }
    }

    pub(super) async fn reconcile_farm(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        request_id: &str,
    ) {
        if matches!(&self.farm.state, FarmRequestState::Uncertain(pending) if pending.request_id == request_id)
            && self.primary_thread_id.is_some()
            && self.current_displayed_thread_id() == self.primary_thread_id
            && !self.chat_widget.is_parent_owned_thread()
        {
            self.send_pending_farm(tui, server).await;
        }
    }

    pub(super) async fn discard_farm(&mut self, tui: &mut tui::Tui, request_id: &str) {
        if self.current_displayed_thread_id() != self.primary_thread_id
            || self.primary_thread_id.is_none()
            || self.chat_widget.is_parent_owned_thread()
        {
            return;
        }
        let pending = match std::mem::take(&mut self.farm.state) {
            FarmRequestState::Uncertain(pending) if pending.request_id == request_id => pending,
            state => {
                self.farm.state = state;
                return;
            }
        };
        if self.current_displayed_thread_id() == Some(pending.thread_id) {
            self.chat_widget.clear_farm_draft(&pending.submission.draft);
        } else if let Some(channel) = self.thread_event_channels.get(&pending.thread_id)
            && let Some(input) = channel.store.lock().await.input_state.as_mut()
        {
            input.clear_farm_draft(&pending.submission.draft);
        }
        if let Some(input) = self
            .agents_overview
            .input_states
            .get_mut(&pending.thread_id)
        {
            input.clear_farm_draft(&pending.submission.draft);
        }
        self.set_farm_receipt(
            tui,
            pending.thread_id,
            request_id,
            "Farm request discarded; a child may still be running. Nothing resent.".into(),
        )
        .await;
    }

    pub(super) fn restore_farm_draft(&mut self, request_id: &str) {
        if let Some(saved) = self.farm.state.retained()
            && saved.request_id == request_id
            && self.current_displayed_thread_id() == Some(saved.thread_id)
            && self.primary_thread_id == Some(saved.thread_id)
            && !self.chat_widget.is_parent_owned_thread()
        {
            if !self.chat_widget.restore_farm_draft(&saved.submission.draft) {
                self.chat_widget.add_error_message(
                    "Clear the current draft before restoring the submitted farm task.".into(),
                );
            }
        } else {
            self.chat_widget.add_error_message(
                "Return to the original main conversation before restoring its farm draft.".into(),
            );
        }
    }

    pub(super) async fn finish_farm(
        &mut self,
        tui: &mut tui::Tui,
        request_id: String,
        result: Result<ThreadFarmResponse, FarmFailure>,
    ) {
        let displayed = self.current_displayed_thread_id();
        let pending = match std::mem::take(&mut self.farm.state) {
            FarmRequestState::Waiting(pending) | FarmRequestState::Uncertain(pending)
                if pending.request_id == request_id =>
            {
                pending
            }
            state => {
                self.farm.state = state;
                return;
            }
        };
        let origin = pending.thread_id;
        let message = match result {
            Ok(response) => {
                if displayed == Some(pending.thread_id) {
                    self.chat_widget.clear_farm_draft(&pending.submission.draft);
                } else if let Some(channel) = self.thread_event_channels.get(&origin)
                    && let Some(input) = channel.store.lock().await.input_state.as_mut()
                {
                    input.clear_farm_draft(&pending.submission.draft);
                }
                if let Some(input) = self.agents_overview.input_states.get_mut(&origin) {
                    input.clear_farm_draft(&pending.submission.draft);
                }
                format!(
                    "Farmed to {} · {}",
                    response.agent_path, response.child_thread_id
                )
            }
            Err(FarmFailure::Rejected(error)) => {
                // Nothing holds a rejected draft: it returns only to a composer that newer
                // input does not occupy.
                if displayed == Some(origin) {
                    self.chat_widget
                        .restore_farm_draft(&pending.submission.draft);
                }
                format!("Farm rejected: {error}.")
            }
            Err(FarmFailure::Uncertain(error)) => {
                self.farm.state = FarmRequestState::Uncertain(pending);
                format!(
                    "Farm outcome unknown; use /farm to check or discard. No retry sent. {error}"
                )
            }
        };
        self.set_farm_receipt(tui, origin, &request_id, message)
            .await;
    }

    async fn set_farm_receipt(
        &mut self,
        tui: &mut tui::Tui,
        thread_id: ThreadId,
        request_id: &str,
        message: String,
    ) {
        let receipt = FarmReceipt {
            thread_id,
            request_id: request_id.into(),
            message,
        };
        if let Some(existing) =
            self.farm.receipts.iter_mut().find(|existing| {
                existing.thread_id == thread_id && existing.request_id == request_id
            })
        {
            *existing = receipt.clone();
        } else {
            if self.farm.receipts.len() == MAX_FARM_RECEIPTS {
                self.farm.receipts.pop_front();
            }
            self.farm.receipts.push_back(receipt.clone());
            // Keep the receipt at its first appearance, not after all replayed output.
            // Status changes replace this same identity in place, including while hidden.
            self.ensure_thread_channel(thread_id)
                .store
                .lock()
                .await
                .push_buffered_event(ThreadBufferedEvent::FarmReceipt {
                    thread_id,
                    request_id: request_id.into(),
                });
        }
        if self.current_displayed_thread_id() == Some(thread_id) {
            self.present_farm_receipt(tui, receipt);
        }
    }

    pub(super) fn queue_farm_receipt_replay(&self) {
        // Only unresolved work needs a fallback if a refreshed/evicted replay buffer
        // lost its original position. Never resurrect old success notices at the tail.
        if let Some(retained) = self.farm.state.retained()
            && self.current_displayed_thread_id() == Some(retained.thread_id)
        {
            self.app_event_tx.send(AppEvent::RestoreFarmReceipt {
                thread_id: retained.thread_id,
                request_id: retained.request_id.clone(),
            });
        }
    }

    pub(super) fn restore_farm_receipt(
        &mut self,
        tui: &mut tui::Tui,
        thread_id: ThreadId,
        request_id: &str,
    ) {
        if self.current_displayed_thread_id() != Some(thread_id) {
            return;
        }
        if let Some(receipt) = self
            .farm
            .receipts
            .iter()
            .find(|receipt| receipt.thread_id == thread_id && receipt.request_id == request_id)
            .cloned()
        {
            self.present_farm_receipt(tui, receipt);
        }
    }

    fn present_farm_receipt(&mut self, tui: &mut tui::Tui, receipt: FarmReceipt) {
        let request_id = receipt.request_id.clone();
        let receipt: Box<dyn HistoryCell> = Box::new(receipt);
        if let Some(index) = self.transcript_cells.iter().position(|cell| {
            cell.as_any()
                .downcast_ref::<FarmReceipt>()
                .is_some_and(|cell| cell.request_id == request_id)
        }) {
            let receipt: Arc<dyn HistoryCell> = receipt.into();
            self.commit_transcript_splice(
                tui,
                TranscriptSplice {
                    range: index..index + 1,
                    cells: vec![receipt],
                    shape: SpliceShape::Revision,
                },
            );
        } else {
            self.insert_history_cell(tui, receipt);
        }
        tui.frame_requester().schedule_frame();
    }
}

#[cfg(test)]
#[path = "farm_tests.rs"]
mod tests;
