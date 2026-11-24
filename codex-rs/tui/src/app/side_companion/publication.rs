//! The passive pane admits completed immutable cells; preparation never runs in paint.
use super::SideCompanion;
use crate::app_event::AppEvent;
use crate::app_event::CapturedSideDisplay;
use crate::app_event_sender::AppEventSender;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::FinalMessageSeparator;
use crate::history_cell::HistoryCell;
use crate::streaming::completion::PreparationToken;
use crate::streaming::completion::PreparedStream;
use codex_app_server_protocol::ThreadItemPayload;
use codex_protocol::ThreadId;
use std::collections::VecDeque;
use std::sync::Arc;

pub(super) struct PublicationQueue {
    pending: VecDeque<PendingCell>,
}

enum PendingCell {
    Stream(PreparedStream),
    Command,
    Ready {
        item: Option<String>,
        cell: Arc<dyn HistoryCell>,
    },
}

impl PublicationQueue {
    pub(super) fn new(display: &mut CapturedSideDisplay) -> Self {
        let mut pending = std::mem::take(&mut display.preparations)
            .into_iter()
            .map(PendingCell::Stream)
            .collect::<VecDeque<_>>();
        if display
            .active
            .as_ref()
            .is_some_and(|cell| cell.as_any().is::<crate::exec_cell::ExecCell>())
        {
            pending.push_back(PendingCell::Command);
        }
        Self { pending }
    }

    pub(super) fn prepare_answer(
        &mut self,
        display: &mut CapturedSideDisplay,
        turn: &str,
        item: &ThreadItemPayload,
        thread_id: ThreadId,
        events: &AppEventSender,
    ) -> bool {
        match item {
            ThreadItemPayload::AgentMessage {
                id,
                text,
                questions,
                phase,
                ..
            } => {
                if !crate::chatwidget::needs_agent_message(
                    display.last_completed_agent_message.as_ref(),
                    turn,
                    item,
                ) {
                    return false;
                }
                let prepared = PreparedStream::pending(thread_id, id.clone());
                let token = prepared.event_token();
                self.pending.push_back(PendingCell::Stream(prepared));
                let source = text.clone();
                let questions = questions.clone().unwrap_or_default();
                let cwd = display.cwd.to_path_buf();
                let visualizations = display.visualizations.clone();
                let events = events.clone();
                tokio::task::spawn_blocking(move || {
                    let markdown = crate::git_action_directives::parse_assistant_markdown(
                        &source,
                        cwd.as_path(),
                    );
                    let cell = Arc::new(AgentMarkdownCell::from_parsed_message(
                        markdown.visible_markdown,
                        source,
                        cwd.as_path(),
                        visualizations,
                        &questions,
                    ));
                    events.send(AppEvent::SideCellPrepared {
                        thread_id,
                        token,
                        cell,
                    });
                });
                display.last_completed_agent_message =
                    Some(crate::chatwidget::CompletedAgentMessage {
                        turn_id: turn.to_owned(),
                        item_id: id.clone(),
                        phase: phase.clone(),
                    });
            }
            _ => return false,
        }
        true
    }

    pub(super) fn complete_turn(&mut self, completion: FinalMessageSeparator) {
        self.pending.push_back(PendingCell::Ready {
            item: None,
            cell: Arc::new(completion),
        });
    }

    pub(super) fn complete_command(
        &mut self,
        display: &mut CapturedSideDisplay,
        active: Box<dyn HistoryCell>,
    ) {
        let ready = PendingCell::Ready {
            item: None,
            cell: Arc::from(active),
        };
        if let Some(slot) = self
            .pending
            .iter_mut()
            .find(|slot| matches!(slot, PendingCell::Command))
        {
            *slot = ready;
        } else {
            self.pending.push_back(ready);
        }
        display.tail.active = None;
        display.source = None;
    }

    pub(super) fn request_publication(&self, thread_id: ThreadId, events: &AppEventSender) {
        if let Some(PendingCell::Ready { cell, .. }) = self.pending.front() {
            events.send(AppEvent::SideCellPrepared {
                thread_id,
                token: PreparationToken::WAKE,
                cell: Arc::clone(cell),
            });
        }
    }

    pub(super) fn owns_preparation(&self, prepared: &PreparedStream) -> bool {
        self.pending.iter().any(
            |slot| matches!(slot, PendingCell::Stream(pending) if pending.same_origin(prepared)),
        )
    }

    pub(super) fn accept_prepared(&mut self, token: PreparationToken, cell: Arc<dyn HistoryCell>) {
        if let Some(PendingCell::Stream(prepared)) = self.pending.iter().find(
            |slot| matches!(slot, PendingCell::Stream(pending) if pending.event_token() == token),
        ) {
            prepared.complete(None, cell);
        }
    }

    pub(super) fn take_ready(&mut self) -> Option<(Option<String>, Arc<dyn HistoryCell>)> {
        match self.pending.front()? {
            PendingCell::Stream(prepared) => {
                let response = prepared.take_ready()?;
                let item = prepared.item_id.clone();
                self.pending.pop_front();
                Some((Some(item), response.cell))
            }
            PendingCell::Ready { .. } => match self.pending.pop_front() {
                Some(PendingCell::Ready { item, cell }) => Some((item, cell)),
                _ => None,
            },
            PendingCell::Command => None,
        }
    }
}

impl crate::app::App {
    pub(in crate::app) fn publish_side_cells(
        &mut self,
        tui: &mut crate::tui::Tui,
        thread_id: ThreadId,
    ) {
        while let Some((item, cell)) = self
            .side_companions
            .get_mut(&thread_id)
            .and_then(SideCompanion::take_ready)
        {
            let companion = &self.side_companions[&thread_id];
            let marker = item.as_ref().and_then(|id| {
                companion.cells.iter().position(|cell| {
                    cell.as_any()
                        .downcast_ref::<crate::history_cell::RespondingCell>()
                        .is_some_and(|response| response.item_id == *id)
                })
            });
            let range = marker.map_or(companion.cells.len()..companion.cells.len(), |index| {
                index..index + 1
            });
            self.commit_transcript_splice(
                tui,
                crate::app::transcript_splice::TranscriptSplice::Companion {
                    thread_id,
                    range,
                    cells: vec![cell],
                },
            );
        }
    }
}
