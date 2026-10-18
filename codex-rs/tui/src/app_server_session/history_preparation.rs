//! Handle-based history effects produce cache observations; the live session alone installs them.

use super::*;
use codex_app_server_client::TypedRequestError;
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

pub(super) async fn merge_thread_item_page(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    page: ThreadItemsListResponse,
    state: &mut ThreadHistoryPagination,
    turns: &mut Vec<Turn>,
) -> Result<Vec<ThreadItem>> {
    let mut missing_turn_ids = page
        .data
        .iter()
        .filter(|entry| !turns.iter().any(|turn| turn.id == entry.turn_id))
        .map(|entry| entry.turn_id.clone())
        .collect::<HashSet<_>>();
    let mut items = Vec::new();
    for entry in page.data {
        while !turns.iter().any(|turn| turn.id == entry.turn_id) {
            let Some(cursor) = state.next_turn_cursor.clone() else {
                return Err(eyre!(
                    "history item {} refers to missing turn {}",
                    entry.item.id(),
                    entry.turn_id
                ));
            };
            // Fetch only the remaining item-backed turns so metadata does not run ahead
            // of this page. Empty turns between them may require another bounded request.
            let limit = missing_turn_ids.len().min(HISTORY_ITEM_PAGE_LIMIT as usize) as u32;
            let page =
                thread_turns_page(policy, handle, thread_id, Some(cursor.clone()), limit).await?;
            for turn in merge_turn_page(&cursor, page, state, turns) {
                missing_turn_ids.remove(&turn.id);
            }
        }
        if let Some(turn) = turns.iter_mut().find(|turn| turn.id == entry.turn_id)
            && !turn.items.iter().any(|item| item.id() == entry.item.id())
        {
            items.push(entry.item.clone());
            turn.items.insert(/*index*/ 0, entry.item);
            turn.items_view = TurnItemsView::Summary;
        }
    }
    // Commit the item frontier only after every item has its turn metadata.
    // A failed metadata request leaves this page retryable; retained items deduplicate.
    state.next_item_cursor = advancing_cursor(
        state.next_item_cursor.as_deref(),
        page.next_cursor,
        &mut state.seen_item_cursors,
    );
    items.reverse();
    Ok(items)
}

pub(in crate::app_server_session) async fn read_thread(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
    thread_id: ThreadId,
    include_turns: bool,
) -> Result<Thread> {
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
        Ok(response) => return Ok(response.thread),
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
    )
    .await;
    result?;
    Ok(response.thread)
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
        let (width, height) = crossterm::terminal::size().unwrap_or(/*default*/ (80, 24));
        let width = width.max(/*other*/ 1);
        let budget = HistoryLoadBudget::new(scope, config, local_settings, height);
        let mut scanned_items = 0;
        let mut rendered_rows = 0;
        while let Some(limit) = budget.next_page_size(rendered_rows, scanned_items) {
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
            let items = merge_thread_item_page(
                policy,
                handle,
                thread_id,
                page,
                &mut state,
                &mut thread.turns,
            )
            .await?;
            // Finish the anchor turn so prompt editing can detect earlier steers,
            // but do not scan history older than the displayed transcript.
            if let HistoryHydrationScope::ThroughTurn(anchor) = scope
                && let Some(index) = thread.turns.iter().position(|turn| turn.id == anchor)
                && thread.turns[..index]
                    .iter()
                    .any(|turn| !turn.items.is_empty())
            {
                break;
            }
            if let Some((config, local_settings)) = config.zip(local_settings) {
                rendered_rows = rendered_history_rows(
                    thread_id,
                    thread,
                    items,
                    config,
                    local_settings,
                    width,
                    rendered_rows,
                );
            } else {
                rendered_rows = rendered_rows.saturating_add(items.len());
            }
            if empty_page || state.next_item_cursor.is_none() {
                break;
            }
        }
        if scope == HistoryHydrationScope::Complete {
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
                merge_turn_page(&cursor, page, &mut state, &mut thread.turns);
            }
        }
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
                .map(|entry| entry.item)
                .collect();
            turn.items_view = TurnItemsView::Full;
        }
    }
    Ok(thread)
}
