//! Conversational gaps end only at a human message or an end-of-turn answer.
//! Completion evidence is independent of rendering: a prepared answer may arrive later.
use std::collections::BTreeMap;
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
use crate::history_cell::activity_parts::FoldGroup;
use crate::history_cell::activity_parts::FoldScope;

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

#[derive(Default)]
pub(super) struct Conversation {
    answers: HashSet<String>,
    boundaries: BTreeMap<usize, String>,
}

impl Conversation {
    pub(super) fn is_boundary(&self, cell: &dyn HistoryCell) -> bool {
        cell.as_any().is::<UserHistoryCell>()
            || cell
                .as_any()
                .downcast_ref::<AgentMarkdownCell>()
                .and_then(|reply| reply.item_id.as_ref())
                .is_some_and(|id| self.answers.contains(id))
    }

    pub(super) fn rebuild(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        self.boundaries = cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| self.is_boundary(cell.as_ref()))
            .map(|(index, cell)| {
                let id = cell
                    .as_any()
                    .downcast_ref::<AgentMarkdownCell>()
                    .and_then(|reply| reply.item_id.as_ref())
                    .map_or_else(
                        || {
                            let user = cell.as_any().downcast_ref::<UserHistoryCell>();
                            if let Some(id) =
                                user.and_then(|user| user.prompt_identity.client_id.as_ref())
                            {
                                format!("conversation:user:client:{id}")
                            } else if let Some(id) = user.and_then(|user| {
                                user.prompt_identity.source_response_item_id.as_ref()
                            }) {
                                format!("conversation:user:source:{id}")
                            } else {
                                format!(
                                    "conversation:user:{:x}",
                                    Arc::as_ptr(cell).cast::<()>() as usize
                                )
                            }
                        },
                        |id| format!("conversation:answer:{id}"),
                    );
                (index, id)
            })
            .collect();
    }

    pub(super) fn control(&self, index: usize) -> Option<&str> {
        self.boundaries.get(&index).map(String::as_str)
    }

    pub(super) fn gap(&self, index: usize) -> Option<&str> {
        if self.boundaries.contains_key(&index) {
            return None;
        }
        Some(
            self.boundaries
                .range(..index)
                .next_back()
                .map_or("conversation:prefix", |(_, id)| id.as_str()),
        )
    }
}

impl TranscriptView {
    pub(crate) fn record_final_response(&mut self, id: String) {
        if self.conversation.answers.insert(id) {
            self.feed.invalidate();
            self.cache.clear();
        }
    }

    pub(super) fn conversation_open(&self, id: &str) -> bool {
        self.disclosure.expanded.is_open_in(
            &FoldScope::Transcript,
            FoldGroup::Conversation,
            id,
            true,
        )
    }

    pub(super) fn conversation_hidden(&self, index: usize) -> bool {
        !self.detailed
            && self.mode == HistoryRenderMode::Rich
            && self
                .conversation
                .gap(index)
                .is_some_and(|id| !self.conversation_open(id))
    }

    pub(crate) fn conversation_tail_hidden(&mut self, cells: &[Arc<dyn HistoryCell>]) -> bool {
        self.refresh_feed(cells, cells.len());
        self.conversation_hidden(cells.len())
    }
}
