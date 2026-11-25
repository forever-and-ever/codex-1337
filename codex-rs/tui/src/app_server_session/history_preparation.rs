//! Handle-based history effects produce cache observations; the live session alone installs them.

use std::collections::HashMap;
use std::future::Future;

use super::AppServerSession;
use super::ClientRequest;
use super::Config;
use super::HISTORY_ITEM_PAGE_LIMIT;
use super::HashSet;
use super::HistoryHydrationScope;
use super::HistoryItem;
use super::HistoryLoadBudget;
use super::INITIAL_HISTORY_TURN_LIMIT;
use super::LocalSettings;
use super::Result;
use super::SortDirection;
use super::Thread;
use super::ThreadHistoryMode;
use super::ThreadHistoryPagination;
use super::ThreadId;
use super::ThreadItem;
use super::ThreadItemsListResponse;
use super::ThreadTurnsListParams;
use super::ThreadTurnsListResponse;
use super::Turn;
use super::TurnItemsView;
use super::WrapErr;
use super::advancing_cursor;
use super::eyre;
use super::rendered_history_rows;
use super::thread_items_page_params;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ThreadItemEntry;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;

pub(crate) enum HistoryPaginationUpdate {
    Untracked,
    Summary(ThreadId),
    Legacy(ThreadId),
    Paginated {
        thread_id: ThreadId,
        state: ThreadHistoryPagination,
    },
}

impl AppServerSession {
    pub(crate) fn apply_history_preparation(&mut self, update: HistoryPaginationUpdate) {
        match update {
            HistoryPaginationUpdate::Untracked => {}
            HistoryPaginationUpdate::Summary(thread_id) => {
                self.history_pagination.remove(&thread_id);
            }
            HistoryPaginationUpdate::Legacy(thread_id) => {
                self.history_pagination.entry(thread_id).or_default();
            }
            HistoryPaginationUpdate::Paginated { thread_id, state } => {
                self.history_pagination.insert(thread_id, state);
            }
        }
    }
}

use super::super::session_preparation::RequestPolicy;

pub(super) async fn thread_items_page(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    turn_id: Option<&str>,
    cursor: Option<String>,
    limit: u32,
) -> Result<ThreadItemsListResponse> {
    let request_id = codex_app_server_protocol::RequestId::String(format!(
        "tui-history-{}",
        uuid::Uuid::new_v4()
    ));
    super::super::session_preparation::request(
        policy,
        handle,
        ClientRequest::ThreadItemsList {
            request_id,
            params: thread_items_page_params(thread_id, turn_id, cursor, limit),
        },
    )
    .await
    .wrap_err("failed to load a bounded thread item page")
}

pub(super) async fn thread_turns_page(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    cursor: Option<String>,
    limit: u32,
) -> Result<ThreadTurnsListResponse> {
    let request_id = codex_app_server_protocol::RequestId::String(format!(
        "tui-history-{}",
        uuid::Uuid::new_v4()
    ));
    super::super::session_preparation::request(
        policy,
        handle,
        ClientRequest::ThreadTurnsList {
            request_id,
            params: ThreadTurnsListParams {
                thread_id: thread_id.to_string(),
                cursor,
                limit: Some(limit),
                sort_direction: Some(SortDirection::Desc),
                items_view: Some(TurnItemsView::NotLoaded),
            },
        },
    )
    .await
    .wrap_err("failed to load a bounded thread history page")
}

// Descending pages append to descending storage. Only turns touched by item
// pages need their item order and ID index changed.
struct HistoryBatch<'a> {
    turns: &'a mut Vec<Turn>,
    positions: HashMap<String, usize>,
    seen_items: HashMap<usize, HashSet<String>>,
}

impl<'a> HistoryBatch<'a> {
    fn new(turns: &'a mut Vec<Turn>) -> Self {
        turns.reverse();
        let positions = turns
            .iter()
            .enumerate()
            .map(|(index, turn)| (turn.id.clone(), index))
            .collect();
        Self {
            turns,
            positions,
            seen_items: HashMap::new(),
        }
    }

    fn has_items_older_than(&self, turn_id: &str) -> bool {
        self.positions.get(turn_id).is_some_and(|&index| {
            self.turns[index + 1..]
                .iter()
                .any(|turn| !turn.items.is_empty())
        })
    }

    fn merge_metadata(
        &mut self,
        cursor: &str,
        page: ThreadTurnsListResponse,
        state: &mut ThreadHistoryPagination,
        mut admitted: impl FnMut(&Turn),
    ) {
        state.next_turn_cursor =
            advancing_cursor(Some(cursor), page.next_cursor, &mut state.seen_turn_cursors);
        let before_page = self.turns.len();
        for turn in page.data {
            if self
                .positions
                .get(&turn.id)
                .is_some_and(|&index| index < before_page)
            {
                continue;
            }
            // Exclude only earlier pages' IDs. For repeats within this page,
            // select the first chronological copy, as with existing turns.
            self.positions.insert(turn.id.clone(), self.turns.len());
            admitted(&turn);
            self.turns.push(turn);
        }
    }

    async fn merge_items<F>(
        &mut self,
        page: ThreadItemsListResponse,
        state: &mut ThreadHistoryPagination,
        mut fetch_turns: impl FnMut(String, u32) -> F,
        mut admitted: impl FnMut(&str, &ThreadItem),
    ) -> Result<()>
    where
        F: Future<Output = Result<ThreadTurnsListResponse>>,
    {
        let mut missing_turn_ids = page
            .data
            .iter()
            .filter(|entry| !self.positions.contains_key(&entry.turn_id))
            .map(|entry| entry.turn_id.clone())
            .collect::<HashSet<_>>();
        for entry in page.data {
            let index = loop {
                if let Some(&index) = self.positions.get(&entry.turn_id) {
                    break index;
                }
                let Some(cursor) = state.next_turn_cursor.clone() else {
                    return Err(eyre!(
                        "history item {} refers to missing turn {}",
                        entry.item.id(),
                        entry.turn_id
                    ));
                };
                // Fetch only the remaining item-backed turns so metadata does not run ahead
                // of this page. Empty turns between them may require another bounded request.
                let limit = u32::try_from(missing_turn_ids.len())
                    .unwrap_or(u32::MAX)
                    .min(HISTORY_ITEM_PAGE_LIMIT);
                let page = fetch_turns(cursor.clone(), limit).await?;
                self.merge_metadata(&cursor, page, state, |turn| {
                    missing_turn_ids.remove(&turn.id);
                });
            };
            state.user_prompt_clocks.record(&entry);
            self.merge_item(index, entry, &mut admitted)?;
        }
        // Leave the item cursor retryable if a page only partially loads.
        state.next_item_cursor = advancing_cursor(
            state.next_item_cursor.as_deref(),
            page.next_cursor,
            &mut state.seen_item_cursors,
        );
        Ok(())
    }

    fn merge_item(
        &mut self,
        index: usize,
        entry: ThreadItemEntry,
        admitted: &mut impl FnMut(&str, &ThreadItem),
    ) -> Result<()> {
        let turn = &mut self.turns[index];
        let seen = self.seen_items.entry(index).or_insert_with(|| {
            turn.items.reverse();
            turn.items.iter().map(|item| item.id().to_owned()).collect()
        });
        if seen.contains(entry.item.id()) {
            return Ok(());
        }
        let item = ThreadItem::try_from(entry)?;
        seen.insert(item.id().to_owned());
        admitted(&turn.id, &item);
        turn.items.push(item);
        turn.items_view = TurnItemsView::Summary;
        Ok(())
    }
}

impl Drop for HistoryBatch<'_> {
    fn drop(&mut self) {
        // Expose chronological turns even when a later page fails.
        for &index in self.seen_items.keys() {
            self.turns[index].items.reverse();
        }
        self.turns.reverse();
    }
}

pub(super) fn merge_thread_turn_page(
    cursor: &str,
    page: ThreadTurnsListResponse,
    state: &mut ThreadHistoryPagination,
    turns: &mut Vec<Turn>,
) -> Vec<Turn> {
    let mut admitted = Vec::new();
    HistoryBatch::new(turns).merge_metadata(cursor, page, state, |turn| {
        admitted.push(turn.clone());
    });
    admitted.reverse();
    admitted
}

pub(super) async fn merge_thread_item_page(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    page: ThreadItemsListResponse,
    state: &mut ThreadHistoryPagination,
    turns: &mut Vec<Turn>,
) -> Result<Vec<HistoryItem>> {
    let mut admitted = Vec::new();
    HistoryBatch::new(turns)
        .merge_items(
            page,
            state,
            |cursor, limit| thread_turns_page(policy, handle, thread_id, Some(cursor), limit),
            |turn_id, item| {
                admitted.push(HistoryItem {
                    turn_id: turn_id.to_owned(),
                    item: item.clone(),
                });
            },
        )
        .await?;
    admitted.reverse();
    Ok(admitted)
}

pub(in crate::app_server_session) async fn read_thread(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    include_turns: bool,
) -> Result<Thread> {
    read_thread_with_receipt(policy, handle, thread_id, include_turns)
        .await
        .map(|(thread, _)| thread)
}

pub(in crate::app_server_session) async fn read_thread_with_receipt(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    include_turns: bool,
) -> Result<(Thread, Option<codex_app_server_protocol::ThreadReadState>)> {
    let request_id = codex_app_server_protocol::RequestId::String(format!(
        "tui-history-{}",
        uuid::Uuid::new_v4()
    ));
    let response = super::super::session_preparation::request::<ThreadReadResponse>(
        policy,
        handle,
        ClientRequest::ThreadRead {
            request_id,
            params: ThreadReadParams {
                thread_id: thread_id.to_string(),
                include_turns,
            },
        },
    )
    .await;
    let mut response: ThreadReadResponse = match response {
        Ok(response) => return Ok((response.thread, response.read_state)),
        Err(TypedRequestError::Server { source, .. })
            if include_turns
                && source.message
                    == "paginated threads do not support thread/read(includeTurns=true)" =>
        {
            let request_id = codex_app_server_protocol::RequestId::String(format!(
                "tui-history-{}",
                uuid::Uuid::new_v4()
            ));
            super::super::session_preparation::request(
                policy,
                handle,
                ClientRequest::ThreadRead {
                    request_id,
                    params: ThreadReadParams {
                        thread_id: thread_id.to_string(),
                        include_turns: false,
                    },
                },
            )
            .await
            .wrap_err("thread/read failed during TUI session lookup")?
        }
        Err(err) => return Err(err).wrap_err("thread/read failed during TUI session lookup"),
    };
    let (result, _) = prepare_initial_thread_history(
        policy,
        handle,
        &mut response.thread,
        /*turn_cursor*/ None,
        /*item_cursor*/ None,
        /*config*/ None,
        /*local_settings*/ None,
        HistoryHydrationScope::Initial,
        crate::history_cell::ImagePathOrigin::Remote, // Metadata-only hydration never projects local images.
    )
    .await;
    result?;
    Ok((response.thread, response.read_state))
}

pub(in crate::app_server_session) async fn prepare_initial_thread_history(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread: &mut Thread,
    turn_cursor: Option<String>,
    item_cursor: Option<String>,
    config: Option<&Config>,
    local_settings: Option<&LocalSettings>,
    scope: HistoryHydrationScope<'_>,
    image_origin: crate::history_cell::ImagePathOrigin,
) -> (Result<()>, HistoryPaginationUpdate) {
    prepare_anchored_thread_history(
        policy,
        handle,
        thread,
        turn_cursor,
        item_cursor,
        config,
        local_settings,
        scope,
        false,
        image_origin,
    )
    .await
}

pub(in crate::app_server_session) async fn prepare_anchored_thread_history(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread: &mut Thread,
    turn_cursor: Option<String>,
    item_cursor: Option<String>,
    config: Option<&Config>,
    local_settings: Option<&LocalSettings>,
    scope: HistoryHydrationScope<'_>,
    anchored: bool,
    image_origin: crate::history_cell::ImagePathOrigin,
) -> (Result<()>, HistoryPaginationUpdate) {
    prepare_history_with_progress(
        policy,
        handle,
        thread,
        turn_cursor,
        item_cursor,
        config,
        local_settings,
        scope,
        &|| {},
        anchored,
        image_origin,
    )
    .await
}

pub(in crate::app_server_session) async fn prepare_history_with_progress(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread: &mut Thread,
    turn_cursor: Option<String>,
    item_cursor: Option<String>,
    config: Option<&Config>,
    local_settings: Option<&LocalSettings>,
    scope: HistoryHydrationScope<'_>,
    progress: &(dyn Fn() + Send + Sync),
    anchored: bool,
    image_origin: crate::history_cell::ImagePathOrigin,
) -> (Result<()>, HistoryPaginationUpdate) {
    let thread_id = match ThreadId::from_string(&thread.id)
        .wrap_err("invalid thread id in bounded history response")
    {
        Ok(thread_id) => thread_id,
        Err(error) => return (Err(error), HistoryPaginationUpdate::Untracked),
    };
    if thread.history_mode == ThreadHistoryMode::Legacy {
        if thread.turns.is_empty() {
            thread.turns = match Box::pin(read_thread(
                policy, handle, thread_id, /*include_turns*/ true,
            ))
            .await
            {
                Ok(thread) => thread.turns,
                Err(error) => return (Err(error), HistoryPaginationUpdate::Untracked),
            };
        }
        return (Ok(()), HistoryPaginationUpdate::Legacy(thread_id));
    }

    let mut state = ThreadHistoryPagination {
        history_mode: ThreadHistoryMode::Paginated,
        next_turn_cursor: turn_cursor,
        next_item_cursor: item_cursor,
        ..ThreadHistoryPagination::default()
    };
    let result = async {
        // A subscribed snapshot with no cursor was empty; an unanchored request would read later live events.
        if !anchored || state.next_turn_cursor.is_some() {
            let page = thread_turns_page(
                policy,
                handle,
                thread_id,
                state.next_turn_cursor.clone(),
                INITIAL_HISTORY_TURN_LIMIT,
            )
            .await?;
            progress();
            thread.turns = page.data.into_iter().rev().collect();
            state.next_turn_cursor = page.next_cursor;
        }
        let (width, height) = crossterm::terminal::size().unwrap_or(/*default*/ (80, 24));
        let width = width.max(/*other*/ 1);
        let budget = HistoryLoadBudget::new(scope, config, local_settings, height);
        let render_budget = budget.rows.and(config.zip(local_settings));
        let mut turns = HistoryBatch::new(&mut thread.turns);
        let mut scanned_items = 0;
        let mut rendered_rows = 0;
        while let Some(limit) = budget.next_page_size(rendered_rows, scanned_items) {
            if anchored && state.next_item_cursor.is_none() {
                break;
            }
            let page = thread_items_page(
                policy,
                handle,
                thread_id,
                /*turn_id*/ None,
                state.next_item_cursor.clone(),
                limit,
            )
            .await?;
            progress();
            let empty_page = page.data.is_empty();
            scanned_items = scanned_items.saturating_add(page.data.len());
            let mut payloads = Vec::new();
            let mut admitted_count = 0;
            turns
                .merge_items(
                    page,
                    &mut state,
                    |cursor, limit| {
                        thread_turns_page(policy, handle, thread_id, Some(cursor), limit)
                    },
                    |_, item| {
                        if render_budget.is_some() {
                            payloads.push(item.payload.clone());
                        } else if budget.rows.is_some() {
                            admitted_count += 1;
                        }
                    },
                )
                .await?;
            // Finish the anchor turn so prompt editing can detect earlier steers,
            // but do not scan history older than the displayed transcript.
            if let HistoryHydrationScope::ThroughTurn(anchor) = scope
                && turns.has_items_older_than(anchor)
            {
                break;
            }
            if empty_page || state.next_item_cursor.is_none() {
                break;
            }
            if let Some((config, local_settings)) = render_budget {
                payloads.reverse();
                rendered_rows = rendered_history_rows(
                    thread_id,
                    thread.cwd.as_path(),
                    payloads,
                    config,
                    local_settings,
                    width,
                    rendered_rows,
                    image_origin,
                );
            } else {
                rendered_rows = rendered_rows.saturating_add(admitted_count);
            }
        }
        if matches!(
            scope,
            HistoryHydrationScope::Complete | HistoryHydrationScope::FullTranscript
        ) {
            while let Some(cursor) = state.next_turn_cursor.clone() {
                let page = thread_turns_page(
                    policy,
                    handle,
                    thread_id,
                    Some(cursor.clone()),
                    HISTORY_ITEM_PAGE_LIMIT,
                )
                .await?;
                progress();
                turns.merge_metadata(&cursor, page, &mut state, |_| {});
            }
        }
        drop(turns);
        super::mark_loaded_interruptions(&mut thread.turns, state.next_item_cursor.as_deref());
        Ok(())
    }
    .await;
    // Attachment and later paging retain every successful page, even if the next one fails.
    let update = if config.is_some() {
        HistoryPaginationUpdate::Paginated { thread_id, state }
    } else {
        HistoryPaginationUpdate::Untracked
    };
    (result, update)
}

pub(in crate::app_server_session) async fn prepare_safety_retry_history(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    turn_id: &str,
) -> Result<Thread> {
    let mut thread = read_thread(policy, handle, thread_id, false).await?;
    if thread.history_mode == ThreadHistoryMode::Legacy {
        let (result, _) = prepare_initial_thread_history(
            policy,
            handle,
            &mut thread,
            None,
            None,
            None,
            None,
            HistoryHydrationScope::Initial,
            crate::history_cell::ImagePathOrigin::Remote, // Metadata-only hydration never projects local images.
        )
        .await;
        result?;
    } else {
        let page =
            thread_turns_page(policy, handle, thread_id, None, INITIAL_HISTORY_TURN_LIMIT).await?;
        thread.turns = page.data.into_iter().rev().collect();
        if let Some(turn_index) = thread.turns.iter().position(|turn| turn.id == turn_id) {
            let page = thread_items_page(
                policy,
                handle,
                thread_id,
                Some(turn_id),
                None,
                HISTORY_ITEM_PAGE_LIMIT,
            )
            .await?;
            if page.next_cursor.is_some() {
                color_eyre::eyre::bail!(
                    "Cannot safely retry a turn whose input exceeds the bounded history page."
                );
            }
            let turn = &mut thread.turns[turn_index];
            turn.items = page
                .data
                .into_iter()
                .rev()
                .map(ThreadItem::try_from)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            turn.items_view = TurnItemsView::Full;
        }
    }
    Ok(thread)
}

#[cfg(test)]
#[path = "history_preparation_tests.rs"]
mod tests;
