use super::App;
use super::transcript_splice::TranscriptSplice;
use super::tui;
use crate::history_cell::HistoryCell;
use crate::history_cell::UserHistoryCell;
use crate::prompt_order::UserPromptIdentity;
use crate::prompt_order::{self};
use std::sync::Arc;

impl App {
    pub(super) fn bind_user_prompt_identity(
        &mut self,
        tui: &mut tui::Tui,
        identity: UserPromptIdentity,
        timestamp: Option<crate::history_cell::HistoryTimestamp>,
    ) {
        let Some(client_id) = identity.client_id.as_deref() else {
            return;
        };
        let Some((index, user)) =
            self.transcript_cells
                .iter()
                .enumerate()
                .find_map(|(index, cell)| {
                    cell.as_any()
                        .downcast_ref::<UserHistoryCell>()
                        .filter(|user| user.prompt_identity.client_id.as_deref() == Some(client_id))
                        .map(|user| (index, user))
                })
        else {
            return;
        };
        if user.prompt_identity == identity
            && timestamp.is_none_or(|timestamp| user.timestamp == Some(timestamp))
        {
            return;
        }
        if user.prompt_identity != identity
            && user.prompt_identity.source_response_item_id.is_some()
        {
            tracing::warn!(client_id, "rejecting conflicting prompt source alias");
            return;
        }
        let mut user = user.clone();
        user.prompt_identity = identity;
        user.timestamp = timestamp.or(user.timestamp);
        self.commit_transcript_splice(
            tui,
            TranscriptSplice::Revision {
                range: index..index + 1,
                cell: Arc::new(user),
            },
        );
        self.reconcile_prompt_order(tui);
    }

    pub(super) fn reconcile_prompt_order(&mut self, tui: &mut tui::Tui) {
        let ordered = prompt_order::reordered(&self.transcript_cells);
        let start = ordered
            .iter()
            .zip(&self.transcript_cells)
            .take_while(|(new, old)| Arc::ptr_eq(new, old))
            .count();
        if start == ordered.len() {
            return;
        }
        let end = ordered.len()
            - ordered[start..]
                .iter()
                .rev()
                .zip(self.transcript_cells[start..].iter().rev())
                .take_while(|(new, old)| Arc::ptr_eq(new, old))
                .count();
        let cells: Vec<Arc<dyn HistoryCell>> = ordered[start..end].to_vec();
        self.commit_transcript_splice(
            tui,
            TranscriptSplice::Members {
                range: start..end,
                cells,
                retained: self.transcript_cells[start..end].to_vec(),
            },
        );
    }
}
