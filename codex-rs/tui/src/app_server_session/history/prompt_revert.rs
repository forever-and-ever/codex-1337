//! Apply prepared prompt-revert pagination receipts.

use super::*;

impl AppServerSession {
    pub(crate) fn apply_prompt_revert(
        &mut self,
        thread_id: ThreadId,
        retained_turns: &[Turn],
        response: &ThreadRevertResponse,
    ) {
        // Older cursors in the retained prefix remain valid. If the entire displayed window
        // disappeared, resume paging at the replacement history's end instead.
        if retained_turns.iter().all(|turn| turn.items.is_empty()) {
            self.history_pagination.insert(
                thread_id,
                ThreadHistoryPagination {
                    history_mode: ThreadHistoryMode::Paginated,
                    next_turn_cursor: response.turns_backwards_cursor.clone(),
                    next_item_cursor: response.items_backwards_cursor.clone(),
                    ..ThreadHistoryPagination::default()
                },
            );
        } else if let Some(page) = self.history_pagination.get_mut(&thread_id) {
            page.loading_older = false;
        }
    }
}
