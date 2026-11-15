//! The unfocused pane projects assistant replies and turn status while the thread event
//! store continues to own replay.

mod publication;
use super::ThreadEventStore;
use crate::app_event::CapturedSideDisplay;
use crate::app_event::CapturedSideWork;
use crate::app_event_sender::AppEventSender;
use crate::history_cell::FinalMessageSeparator;
use crate::history_cell::HistoryCell;
use crate::streaming::completion::PreparationToken;
use crate::streaming::completion::PreparedStream;
use crate::transcript_view::TranscriptView;
use chrono::Local;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::ThreadId;
use publication::PublicationQueue;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

pub(super) struct SideCompanion {
    pub(super) display: CapturedSideDisplay,
    pub(super) cells: Vec<Arc<dyn HistoryCell>>,
    pub(super) view: TranscriptView,
    publication: PublicationQueue,
}

impl SideCompanion {
    pub(super) fn new(
        mut display: CapturedSideDisplay,
        cells: Vec<Arc<dyn HistoryCell>>,
        view: TranscriptView,
    ) -> Self {
        let publication = PublicationQueue::new(&mut display);
        Self {
            display,
            cells,
            view,
            publication,
        }
    }

    pub(super) fn apply_notification(
        &mut self,
        notification: &ServerNotification,
        store: &ThreadEventStore,
        thread_id: ThreadId,
        events: &AppEventSender,
        new_status: impl FnOnce() -> crate::status_indicator_widget::StatusIndicatorWidget,
    ) -> bool {
        match notification {
            ServerNotification::TurnStarted(event)
                if self
                    .display
                    .work
                    .as_ref()
                    .is_none_or(|work| work.turn_id != event.turn.id) =>
            {
                self.display.work = Some(CapturedSideWork::new(
                    event.turn.id.clone(),
                    new_status(),
                    Duration::ZERO,
                ));
            }
            ServerNotification::ItemCompleted(event) => {
                let item = &event.item.payload;
                if store.is_realtime_delegated_turn(&event.turn_id)
                    && crate::chatwidget::is_private_realtime_agent_item(item)
                {
                    return false;
                }
                let changed = self.finish_item(&event.turn_id, item, thread_id, events);
                self.publication.request_publication(thread_id, events);
                return changed;
            }
            ServerNotification::TurnCompleted(event)
                if event.turn.status != TurnStatus::InProgress =>
            {
                let turn = &event.turn;
                for item in turn.item_payloads() {
                    if matches!(item, ThreadItemPayload::CommandExecution { .. }) {
                        self.finish_item(&turn.id, item, thread_id, events);
                    }
                }
                if turn.status == TurnStatus::Completed
                    && let Some((item, _)) = crate::chatwidget::final_agent_message(
                        turn,
                        store.is_realtime_delegated_turn(&turn.id),
                    )
                    && crate::chatwidget::needs_agent_message(
                        self.display.last_completed_agent_message.as_ref(),
                        &turn.id,
                        item,
                    )
                {
                    self.finish_item(&turn.id, item, thread_id, events);
                }
                let work = self.display.work.take_if(|work| work.turn_id == turn.id);
                if turn.status == TurnStatus::Completed
                    && (work.is_some() || store.active_turn_id() == Some(turn.id.as_str()))
                    && let Some(completion) = FinalMessageSeparator::from_turn_once(
                        turn,
                        &mut self.display.rendered_completion_turn_ids,
                        work.map(|work| work.timer.elapsed_at(Instant::now())),
                        Some(Local::now()),
                    )
                {
                    self.publication.complete_turn(completion);
                }
            }
            ServerNotification::ThreadClosed(_) => return self.display.work.take().is_some(),
            _ => return false,
        }
        self.publication.request_publication(thread_id, events);
        true
    }

    fn finish_item(
        &mut self,
        turn: &str,
        item: &ThreadItemPayload,
        thread_id: ThreadId,
        events: &AppEventSender,
    ) -> bool {
        match item {
            ThreadItemPayload::AgentMessage { .. } => {
                return self.publication.prepare_answer(
                    &mut self.display,
                    turn,
                    item,
                    thread_id,
                    events,
                );
            }
            ThreadItemPayload::CommandExecution {
                id,
                status,
                exit_code,
                source,
                aggregated_output,
                duration_ms,
                ..
            } => {
                let Some(cell) = self.display.active.as_mut().and_then(|active| {
                    active
                        .as_any_mut()
                        .downcast_mut::<crate::exec_cell::ExecCell>()
                }) else {
                    return false;
                };
                if *status == codex_app_server_protocol::CommandExecutionStatus::InProgress
                    || !cell.complete_call(
                        id,
                        crate::exec_cell::CommandOutput::from_execution(
                            status.clone(),
                            *exit_code,
                            *source,
                            aggregated_output.clone(),
                        ),
                        Duration::from_millis(duration_ms.unwrap_or_default().max(0) as u64),
                    )
                {
                    return false;
                }
                if !cell.is_active()
                    && let Some(active) = self.display.active.take()
                {
                    self.publication.complete_command(&mut self.display, active);
                }
            }
            _ => return false,
        }
        true
    }

    pub(super) fn owns_preparation(&self, prepared: &PreparedStream) -> bool {
        self.publication.owns_preparation(prepared)
    }

    pub(super) fn accept_prepared(&mut self, token: PreparationToken, cell: Arc<dyn HistoryCell>) {
        self.publication.accept_prepared(token, cell);
    }

    pub(super) fn take_ready(&mut self) -> Option<(Option<String>, Arc<dyn HistoryCell>)> {
        self.publication.take_ready()
    }
}
