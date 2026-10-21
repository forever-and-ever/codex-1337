//! The dialogue is the user's prompts and each turn's final answer; everything else a turn shows
//! is its work, which dialogue mode hides from the compact transcript.
//! Completion evidence is independent of rendering: a prepared answer may arrive later.
use std::collections::HashSet;
use std::sync::Arc;

use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::models::MessagePhase;

use super::TranscriptView;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::history_cell::UserHistoryCell;
use crate::history_cell::status_output;

pub(crate) fn final_response(turn: &Turn) -> Option<&str> {
    (turn.status == TurnStatus::Completed).then_some(())?;
    turn.items.iter().rev().find_map(|item| match item {
        ThreadItem::AgentMessage {
            id,
            phase: Some(MessagePhase::FinalAnswer) | None,
            ..
        } => Some(id.as_str()),
        _ => None,
    })
}

/// What the compact transcript shows of each turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Lens {
    #[default]
    Work,
    Dialogue,
}

#[derive(Default)]
pub(super) struct Dialogue {
    answers: HashSet<String>,
}

impl Dialogue {
    pub(super) fn admits(&self, cell: &dyn HistoryCell) -> bool {
        cell.as_any().is::<UserHistoryCell>()
            || cell
                .as_any()
                .downcast_ref::<AgentMarkdownCell>()
                .and_then(|reply| reply.item_id.as_ref())
                .is_some_and(|id| self.answers.contains(id))
    }
}

impl TranscriptView {
    pub(crate) fn record_final_response(&mut self, id: String) {
        if self.dialogue.answers.insert(id) {
            self.feed.invalidate();
            self.cache.clear();
        }
    }

    /// Whether the lens hides the work; detailed and raw presentations show all of it.
    pub(crate) fn dialogue_only(&self) -> bool {
        self.lens == Lens::Dialogue && !self.detailed && self.mode == HistoryRenderMode::Rich
    }

    pub(crate) fn toggle_lens(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        let lens = match self.lens {
            Lens::Work => Lens::Dialogue,
            Lens::Dialogue => Lens::Work,
        };
        self.set_lens(cells, lens);
    }

    pub(super) fn set_lens(&mut self, cells: &[Arc<dyn HistoryCell>], lens: Lens) {
        if self.lens == lens {
            return;
        }
        self.end_selection(cells);
        self.lens = lens;
        self.cache.clear();
        self.suppressed_prompt_header = None;
        self.live_key = None;
        self.live_separated = None;
    }

    /// Whether the lens hides this entry. `/status` is an inspection the lens keeps.
    pub(super) fn eclipsed(&self, cells: &[Arc<dyn HistoryCell>], index: usize) -> bool {
        self.dialogue_only()
            && !cells
                .get(index)
                .is_some_and(|cell| status_output::matches(cell.as_ref()))
            && !cells
                .get(self.feed.response_owner(index).unwrap_or(index))
                .is_some_and(|cell| self.dialogue.admits(cell.as_ref()))
    }
}
