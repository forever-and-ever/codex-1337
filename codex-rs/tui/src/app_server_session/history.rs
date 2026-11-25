//! Bounded app-server transcript loading for resume, fork, and transcript views.

use color_eyre::eyre::eyre;

use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemPayload;
use std::collections::HashSet;
use std::path::Path;

use super::AppServerSession;
use crate::history_cell::HistoryRenderMode;
use crate::legacy_core::config::Config;
use crate::local_settings::LocalSettings;
use crate::resize_reflow_cap::resize_reflow_max_rows;
use crate::thread_transcript::RawReasoningVisibility;
use crate::thread_transcript::thread_items_to_transcript_cells;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::SortDirection;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadItemsListCursor;
use codex_app_server_protocol::ThreadItemsListParams;
use codex_app_server_protocol::ThreadItemsListResponse;
use codex_app_server_protocol::ThreadRevertResponse;
use codex_app_server_protocol::ThreadTurnsListParams;
use codex_app_server_protocol::ThreadTurnsListResponse;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;

pub(crate) const INITIAL_HISTORY_TURN_LIMIT: u32 = 5;
pub(crate) const HISTORY_ITEM_PAGE_LIMIT: u32 = 100;
pub(crate) const HISTORY_ITEM_SCAN_LIMIT: usize = 4 * HISTORY_ITEM_PAGE_LIMIT as usize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryHydrationScope<'a> {
    Initial,
    Complete,
    /// Include older turns that have no items.
    FullTranscript,
    ThroughTurn(&'a str),
}

/// Limits initial hydration independently from the number of items retained by later paging.
struct HistoryLoadBudget {
    rows: Option<usize>,
    items: Option<usize>,
}

impl HistoryLoadBudget {
    fn new(
        scope: HistoryHydrationScope<'_>,
        config: Option<&Config>,
        local_settings: Option<&crate::local_settings::LocalSettings>,
        terminal_height: u16,
    ) -> Self {
        let transcript_mode = local_settings
            .map(|settings| settings.transcript_mode)
            .or_else(|| config.map(|config| LocalSettings::from(config).transcript_mode));
        if scope == HistoryHydrationScope::Initial
            && transcript_mode.is_some_and(crate::transcript_mode::TranscriptMode::is_owned)
        {
            // A few screenfuls cover the first viewport and nearby reading without coupling
            // owned history to terminal scrollback settings, including unlimited scrollback.
            return Self {
                rows: Some(usize::from(terminal_height.max(/*other*/ 1)) * 3),
                items: Some(HISTORY_ITEM_SCAN_LIMIT),
            };
        }
        let rows = local_settings
            .and_then(|settings| resize_reflow_max_rows(settings.terminal_resize_reflow()));
        let items = match (scope, config, rows) {
            (HistoryHydrationScope::Complete | HistoryHydrationScope::FullTranscript, _, _)
            | (HistoryHydrationScope::ThroughTurn(_), _, _)
            | (HistoryHydrationScope::Initial, Some(_), None) => None,
            (HistoryHydrationScope::Initial, Some(_), Some(max_rows)) => {
                Some(max_rows.saturating_add(HISTORY_ITEM_SCAN_LIMIT))
            }
            (HistoryHydrationScope::Initial, None, _) => Some(HISTORY_ITEM_PAGE_LIMIT as usize),
        };
        Self { rows, items }
    }

    fn next_page_size(&self, rendered_rows: usize, scanned_items: usize) -> Option<u32> {
        let remaining_rows = self.rows.map(|budget| budget.saturating_sub(rendered_rows));
        let remaining_items = self
            .items
            .map(|budget| budget.saturating_sub(scanned_items));
        if remaining_rows == Some(0) || remaining_items == Some(0) {
            return None;
        }
        let limit = remaining_items
            .unwrap_or(HISTORY_ITEM_PAGE_LIMIT as usize)
            .min(HISTORY_ITEM_PAGE_LIMIT as usize);
        // After the first page, hidden items must not reduce requests to one item.
        let limit = if scanned_items != 0 {
            limit
        } else {
            limit.min(remaining_rows.unwrap_or(limit))
        };
        Some(limit as u32)
    }
}

pub(crate) fn thread_items_page_params(
    thread_id: ThreadId,
    turn_id: Option<&str>,
    cursor: Option<String>,
    limit: u32,
) -> ThreadItemsListParams {
    ThreadItemsListParams {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.map(str::to_string),
        cursor: cursor.map(ThreadItemsListCursor::Opaque),
        limit: Some(limit),
        sort_direction: Some(SortDirection::Desc),
        defer_output: None,
    }
}

fn advancing_cursor(
    current: Option<&str>,
    next: Option<String>,
    seen_cursors: &mut HashSet<String>,
) -> Option<String> {
    if let Some(current) = current {
        seen_cursors.insert(current.to_string());
    }
    next.filter(|next| seen_cursors.insert(next.clone()))
}

/// One frontier, carried through request, response, and admission without confusing cursor domains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HistoryPage<Items, Turns> {
    Items(Items),
    Turns(Turns),
}

pub(crate) type HistoryCursor = HistoryPage<String, String>;
pub(crate) type HistoryResponse = HistoryPage<ThreadItemsListResponse, ThreadTurnsListResponse>;
pub(crate) type HistoryContent = HistoryPage<Vec<HistoryItem>, Vec<Turn>>;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryItem {
    pub(crate) turn_id: String,
    pub(crate) item: ThreadItem,
}

impl HistoryItem {
    pub(crate) fn key(&self) -> (&str, &str) {
        (&self.turn_id, self.item.id())
    }
}

impl HistoryCursor {
    pub(crate) async fn load(
        &self,
        handle: &codex_app_server_client::AppServerRequestHandle,
        request_id: codex_app_server_protocol::RequestId,
        thread_id: ThreadId,
        limit: u32,
    ) -> std::result::Result<HistoryResponse, codex_app_server_client::TypedRequestError> {
        match self {
            Self::Items(cursor) => handle
                .request_typed(ClientRequest::ThreadItemsList {
                    request_id,
                    params: thread_items_page_params(thread_id, None, Some(cursor.clone()), limit),
                })
                .await
                .map(HistoryPage::Items),
            Self::Turns(cursor) => handle
                .request_typed(ClientRequest::ThreadTurnsList {
                    request_id,
                    params: ThreadTurnsListParams {
                        thread_id: thread_id.to_string(),
                        cursor: Some(cursor.clone()),
                        limit: Some(limit),
                        sort_direction: Some(SortDirection::Desc),
                        items_view: Some(TurnItemsView::NotLoaded),
                    },
                })
                .await
                .map(HistoryPage::Turns),
        }
    }
}

impl ThreadHistoryPagination {
    fn advance_turn_cursor(&mut self, cursor: &str, next: Option<String>) {
        self.next_turn_cursor = advancing_cursor(Some(cursor), next, &mut self.seen_turn_cursors);
    }

    fn next_page(&self) -> Option<HistoryCursor> {
        self.next_item_cursor
            .clone()
            .map(HistoryPage::Items)
            .or_else(|| self.next_turn_cursor.clone().map(HistoryPage::Turns))
    }
}

fn merge_turn_page(
    cursor: &str,
    page: ThreadTurnsListResponse,
    state: &mut ThreadHistoryPagination,
    turns: &mut Vec<Turn>,
) -> Vec<Turn> {
    state.advance_turn_cursor(cursor, page.next_cursor);
    let admitted = page
        .data
        .into_iter()
        .rev()
        .filter(|turn| !turns.iter().any(|known| known.id == turn.id))
        .collect::<Vec<_>>();
    turns.splice(0..0, admitted.iter().cloned());
    admitted
}

// Turn metadata can extend before the loaded item window. Inside the window an empty
// interrupted turn is genuinely itemless and its notice is ready to replay.
fn mark_loaded_interruptions(turns: &mut [Turn], older_cursor: Option<&str>) {
    let mut in_window = older_cursor.is_none();
    for turn in turns {
        in_window |= !turn.items.is_empty();
        if in_window
            && turn.status == TurnStatus::Interrupted
            && turn.items_view == TurnItemsView::NotLoaded
        {
            turn.items_view = TurnItemsView::Summary;
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ThreadHistoryPagination {
    pub(super) history_mode: ThreadHistoryMode,
    next_turn_cursor: Option<String>,
    next_item_cursor: Option<String>,
    seen_turn_cursors: HashSet<String>,
    seen_item_cursors: HashSet<String>,
    loading_older: bool,
    user_prompt_clocks: crate::prompt_order::UserPromptClocks,
}

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

    pub(crate) fn user_prompt_clocks(
        &self,
        thread_id: ThreadId,
    ) -> Option<&crate::prompt_order::UserPromptClocks> {
        self.history_pagination
            .get(&thread_id)
            .map(|state| &state.user_prompt_clocks)
    }

    pub(crate) fn has_older_history(&self, thread_id: ThreadId) -> bool {
        self.history_pagination
            .get(&thread_id)
            .is_some_and(|page| page.next_page().is_some())
    }

    pub(crate) fn begin_older_history_page(
        &mut self,
        thread_id: ThreadId,
    ) -> Option<HistoryCursor> {
        let page = self.history_pagination.get_mut(&thread_id)?;
        if page.loading_older {
            return None;
        }
        let cursor = page.next_page()?;
        page.loading_older = true;
        Some(cursor)
    }

    pub(crate) fn has_pending_older_history_page(&self, thread_id: ThreadId) -> bool {
        self.history_pagination
            .get(&thread_id)
            .is_some_and(|page| page.loading_older)
    }

    /// Match a completion to the page still pending for this thread, before interpreting its result.
    pub(crate) fn is_older_history_page_pending(
        &self,
        thread_id: ThreadId,
        cursor: &HistoryCursor,
    ) -> bool {
        self.history_pagination
            .get(&thread_id)
            .is_some_and(|page| page.loading_older && page.next_page().as_ref() == Some(cursor))
    }

    pub(crate) fn cancel_older_history_page(
        &mut self,
        thread_id: ThreadId,
        cursor: &HistoryCursor,
    ) {
        if let Some(page) = self.history_pagination.get_mut(&thread_id)
            && page.next_page().as_ref() == Some(cursor)
        {
            page.loading_older = false;
        }
    }

    pub(crate) async fn apply_older_history_page(
        &mut self,
        thread_id: ThreadId,
        cursor: &HistoryCursor,
        page: HistoryResponse,
        turns: &mut Vec<Turn>,
    ) -> Result<HistoryContent> {
        if !self.is_older_history_page_pending(thread_id, cursor) {
            return Ok(HistoryPage::Items(Vec::new()));
        }
        let Some(mut state) = self.history_pagination.get(&thread_id).cloned() else {
            return Ok(HistoryPage::Items(Vec::new()));
        };
        let admitted = match (cursor, page) {
            (HistoryPage::Items(_), HistoryPage::Items(page)) => HistoryPage::Items(
                self.merge_thread_item_page(thread_id, page, &mut state, turns)
                    .await?,
            ),
            (HistoryPage::Turns(cursor), HistoryPage::Turns(page)) => {
                HistoryPage::Turns(merge_turn_page(cursor, page, &mut state, turns))
            }
            _ => return Err(eyre!("history response does not match its cursor domain")),
        };
        mark_loaded_interruptions(turns, state.next_item_cursor.as_deref());
        state.loading_older = false;
        self.history_pagination.insert(thread_id, state);
        Ok(admitted)
    }

    pub(crate) async fn thread_items_page(
        &mut self,
        thread_id: ThreadId,
        turn_id: Option<&str>,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<ThreadItemsListResponse> {
        preparation::thread_items_page(
            super::session_preparation::RequestPolicy::Unbounded,
            &self.request_handle(),
            thread_id,
            turn_id,
            cursor,
            limit,
        )
        .await
    }

    async fn merge_thread_item_page(
        &mut self,
        thread_id: ThreadId,
        page: ThreadItemsListResponse,
        state: &mut ThreadHistoryPagination,
        turns: &mut Vec<Turn>,
    ) -> Result<Vec<HistoryItem>> {
        preparation::merge_thread_item_page(
            super::session_preparation::RequestPolicy::Unbounded,
            &self.request_handle(),
            thread_id,
            page,
            state,
            turns,
        )
        .await
    }

    /// Hydrates paginated threads through bounded turn and item pages.
    ///
    /// Legacy servers expose neither paging method, so only legacy threads may
    /// use `thread/read(includeTurns: true)` to preserve their existing history.
    pub(crate) async fn hydrate_initial_thread_history(
        &mut self,
        thread: &mut Thread,
        turn_cursor: Option<String>,
        item_cursor: Option<String>,
        config: Option<&Config>,
        local_settings: Option<&crate::local_settings::LocalSettings>,
        scope: HistoryHydrationScope<'_>,
    ) -> Result<()> {
        let image_origin = if self.uses_remote_workspace() {
            crate::history_cell::ImagePathOrigin::Remote
        } else {
            crate::history_cell::ImagePathOrigin::Local
        };
        let (result, update) = preparation::prepare_initial_thread_history(
            super::session_preparation::RequestPolicy::Unbounded,
            &self.request_handle(),
            thread,
            turn_cursor,
            item_cursor,
            config,
            local_settings,
            scope,
            image_origin,
        )
        .await;
        self.apply_history_preparation(update);
        result
    }
}

fn rendered_history_rows(
    thread_id: ThreadId,
    cwd: &Path,
    items: Vec<ThreadItemPayload>,
    config: &Config,
    local_settings: &crate::local_settings::LocalSettings,
    width: u16,
    rendered_rows: usize,
    image_origin: crate::history_cell::ImagePathOrigin,
) -> usize {
    let visibility = if config.show_raw_agent_reasoning {
        RawReasoningVisibility::Visible
    } else {
        RawReasoningVisibility::Hidden
    };
    let mode = if local_settings.tui.raw_output_mode {
        HistoryRenderMode::Raw
    } else {
        HistoryRenderMode::Rich
    };
    thread_items_to_transcript_cells(
        Some(thread_id),
        cwd,
        items,
        visibility,
        Some(config),
        &|_| crate::multi_agents::AgentMetadata::default(),
        image_origin,
    )
    .into_iter()
    .fold(rendered_rows, |rows, cell| {
        let height = usize::from(cell.desired_height_for_mode(width, mode));
        rows + height + usize::from(height != 0 && rows != 0 && !cell.is_stream_continuation())
    })
}

#[path = "history_preparation.rs"]
pub(super) mod preparation;
#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
