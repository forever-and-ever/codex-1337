//! Farm is a root-thread RPC, never a queued turn or steer. Refuse UI replacement
//! while its identity or saved draft remains; loss of a response is not a failed spawn.

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
    uncertain: bool,
}

#[derive(Default)]
pub(super) struct FarmUi {
    pub(super) pending: Option<RetainedFarm>,
    pub(super) saved: Option<RetainedFarm>,
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
        if self.farm.saved.is_some() {
            self.chat_widget.add_error_message(
                "Restore/discard the saved farm draft before delegating another task.".into(),
            );
            return;
        }
        if let Some(pending) = &self.farm.pending {
            self.chat_widget.add_error_message(if pending.uncertain {
                format!("Farm outcome is unknown; use /farm to check or discard. Request {} is retained.", pending.request_id)
            } else {
                "A farm request is already waiting for its receipt.".into()
            });
            return;
        }
        let request_id = Uuid::new_v4().to_string();
        self.farm.pending = Some(RetainedFarm {
            thread_id,
            request_id,
            mode,
            submission,
            uncertain: false,
        });
        self.send_pending_farm(tui, server).await;
    }

    async fn send_pending_farm(&mut self, tui: &mut tui::Tui, server: &mut AppServerSession) {
        let Some(pending) = self.farm.pending.as_mut() else {
            return;
        };
        pending.uncertain = false;
        let request_id = pending.request_id.clone();
        let origin = pending.thread_id;
        let params = ThreadFarmParams {
            thread_id: pending.thread_id.to_string(),
            request_id: request_id.clone(),
            mode: pending.mode,
            input: pending.submission.input.clone(),
        };
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
        match &self.farm.pending {
            Some(pending) if pending.uncertain => {
                self.chat_widget.open_farm_recovery(
                    pending.request_id.clone(),
                    crate::chatwidget::FarmRecovery::MissingReceipt,
                );
            }
            Some(_) => self
                .chat_widget
                .add_error_message("A farm request is still waiting for its receipt.".into()),
            None => match &self.farm.saved {
                Some(saved) => self.chat_widget.open_farm_recovery(
                    saved.request_id.clone(),
                    crate::chatwidget::FarmRecovery::SavedDraft,
                ),
                None => self
                    .chat_widget
                    .add_error_message("Usage: /farm <task> or /farm_fresh <task>".into()),
            },
        }
    }

    pub(super) async fn reconcile_farm(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        request_id: &str,
    ) {
        if self.farm.pending.as_ref().is_some_and(|pending| {
            pending.request_id == request_id
                && pending.uncertain
                && self.primary_thread_id.is_some()
                && self.current_displayed_thread_id() == self.primary_thread_id
                && !self.chat_widget.is_parent_owned_thread()
        }) {
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
        let pending = if self
            .farm
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id && pending.uncertain)
        {
            self.farm.pending.take()
        } else if self
            .farm
            .saved
            .as_ref()
            .is_some_and(|saved| saved.request_id == request_id)
        {
            self.farm.saved.take()
        } else {
            None
        };
        if let Some(pending) = pending {
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
    }

    pub(super) fn restore_farm_draft(&mut self, request_id: &str) {
        let pending = self
            .farm
            .pending
            .as_ref()
            .filter(|pending| pending.request_id == request_id);
        let saved = pending.or_else(|| {
            self.farm
                .saved
                .as_ref()
                .filter(|saved| saved.request_id == request_id)
        });
        if let Some(saved) = saved
            && saved.request_id == request_id
            && self.current_displayed_thread_id() == Some(saved.thread_id)
            && self.primary_thread_id == Some(saved.thread_id)
            && !self.chat_widget.is_parent_owned_thread()
        {
            if self.chat_widget.restore_farm_draft(&saved.submission.draft) && pending.is_none() {
                self.farm.saved = None;
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
        let Some(pending) = self
            .farm
            .pending
            .as_mut()
            .filter(|pending| pending.request_id == request_id)
        else {
            return;
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
                self.farm.pending = None;
                format!(
                    "Farmed to {} · {}",
                    response.agent_path, response.child_thread_id
                )
            }
            Err(FarmFailure::Rejected(error)) => {
                self.farm.saved = self.farm.pending.take();
                format!("Farm rejected: {error}. Submitted draft retained; use /farm to restore.")
            }
            Err(FarmFailure::Uncertain(error)) => {
                pending.uncertain = true;
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
        for retained in self.farm.pending.iter().chain(self.farm.saved.iter()) {
            if self.current_displayed_thread_id() == Some(retained.thread_id) {
                self.app_event_tx.send(AppEvent::RestoreFarmReceipt {
                    thread_id: retained.thread_id,
                    request_id: retained.request_id.clone(),
                });
            }
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
            if !tui.is_owned_screen()
                && !self.native_history.contains(&self.transcript_cells[index])
            {
                let width = self
                    .chat_widget
                    .history_wrap_width(tui.terminal.last_known_screen_size.width);
                let lines = receipt.display_hyperlink_lines_for_mode(
                    width,
                    self.chat_widget.history_render_mode(),
                );
                let previous = self
                    .last_rendered_history_tail
                    .as_ref()
                    .filter(|tail| {
                        tail.cell
                            .upgrade()
                            .is_some_and(|cell| Arc::ptr_eq(&cell, &self.transcript_cells[index]))
                    })
                    .map(|tail| tail.lines.clone());
                let replaced = previous.is_some_and(|previous| {
                    tui.replace_visible_history_tail(
                        &previous,
                        &lines,
                        self.history_line_wrap_policy(),
                    )
                    .unwrap_or(false)
                });
                if !replaced {
                    self.insert_history_cell_lines(tui, receipt.as_ref(), width);
                }
                self.last_rendered_history_tail = Some(history_ui::RenderedHistoryTail {
                    cell: Arc::downgrade(&receipt),
                    lines,
                });
            }
            self.native_history
                .consolidate(&self.transcript_cells[index..=index], &receipt);
            self.transcript_view
                .replace_range(&self.transcript_cells, index..index + 1, &receipt);
            self.transcript_cells[index] = receipt;
            if let Some(Overlay::Transcript(overlay)) = &mut self.overlay {
                overlay.replace_cells(self.transcript_cells.clone());
            }
        } else {
            self.insert_history_cell(tui, receipt);
        }
        tui.frame_requester().schedule_frame();
    }
}

#[cfg(test)]
#[path = "farm_tests.rs"]
mod tests;
