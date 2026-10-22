//! The dialogue is the user's prompts and the agent's replies; everything else a turn shows is
//! its work, which dialogue mode hides from the compact transcript. A final answer is a reply,
//! and so is any message no final answer follows before the next prompt: what a running,
//! interrupted or steered turn has said stands until an answer supersedes it.
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
    /// Whether this is a final answer, which supersedes the messages before it.
    pub(super) fn closes(&self, cell: &dyn HistoryCell) -> bool {
        cell.as_any()
            .downcast_ref::<AgentMarkdownCell>()
            .and_then(|reply| reply.item_id.as_ref())
            .is_some_and(|id| self.answers.contains(id))
    }

    /// Which cells the lens keeps: prompts, final answers, and the messages no final answer
    /// follows before the next prompt.
    pub(super) fn keeps(&self, cells: &[Arc<dyn HistoryCell>]) -> Vec<bool> {
        let mut answered = false;
        let mut kept = vec![false; cells.len()];
        for (index, cell) in cells.iter().enumerate().rev() {
            let any = cell.as_any();
            if any.is::<UserHistoryCell>() {
                answered = false;
                kept[index] = true;
            } else if any.is::<AgentMarkdownCell>() {
                let closes = self.closes(cell.as_ref());
                kept[index] = closes || !answered;
                answered |= closes;
            }
        }
        kept
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

    /// Rewind chooses among prompts, so it hides the work between them; the lens outlives it.
    pub(crate) fn show_dialogue(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        self.set_lens(cells, Lens::Dialogue);
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
            && !self
                .feed
                .in_dialogue(self.feed.response_owner(index).unwrap_or(index))
    }
}
