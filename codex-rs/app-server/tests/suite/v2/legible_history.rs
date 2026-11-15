//! Builds of other ages share one history store, so a stored item may have a kind this build
//! cannot read. History reads omit such an item instead of failing.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::SortDirection;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadItemsListParams;
use codex_app_server_protocol::ThreadItemsListResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadTurnsListParams;
use codex_app_server_protocol::ThreadTurnsListResponse;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[tokio::test]
async fn history_reads_omit_items_this_build_cannot_read() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    mcp.initialize_with_capabilities(
        ClientInfo {
            name: "test-client".to_string(),
            title: None,
            version: "0.1.0".to_string(),
        },
        Some(InitializeCapabilities {
            required_human_approval: false,
            explicit_gateway_oauth: false,
            experimental_api: true,
            request_attestation: false,
            opt_out_notification_methods: None,
            mcp_server_openai_form_elicitation: false,
            extensions: None,
        }),
    )
    .await?;
    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            history_mode: Some(ThreadHistoryMode::Paginated),
            ..Default::default()
        })
        .await?;
    let completed = mcp
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let saved = newest_items(&mut mcp, &thread.id, /*limit*/ None)
        .await?
        .data;

    // A later build sharing this history store wrote the newest item, of a kind this one lacks.
    let sqlite = codex_state::SqliteConfig::new_for_testing(codex_home.path().abs());
    let pool = sqlite
        .open_read_write_pool(&sqlite.thread_history_db_path())
        .await?;
    sqlx::query(
        "INSERT INTO thread_items \
         (thread_id, turn_id, item_id, rollout_ordinal, updated_at_ordinal, created_at_ms, \
          item_type, item_json) \
         VALUES (?, ?, 'later', 1000000, 1000000, 0, 'fromALaterBuild', \
          '{\"type\":\"fromALaterBuild\",\"id\":\"later\"}')",
    )
    .bind(&thread.id)
    .bind(&completed.turn.id)
    .execute(&pool)
    .await?;
    pool.close().await;

    // A page holding only that item is not the end of the history.
    let newest = newest_items(&mut mcp, &thread.id, Some(1)).await?;
    assert_eq!(newest.data, saved[..1]);
    let ThreadTurnsListResponse { data: turns, .. } = mcp
        .request(|request_id| ClientRequest::ThreadTurnsList {
            request_id,
            params: ThreadTurnsListParams {
                thread_id: thread.id.clone(),
                cursor: None,
                limit: None,
                sort_direction: None,
                items_view: Some(TurnItemsView::Full),
            },
        })
        .await?;
    assert_eq!(
        turns
            .into_iter()
            .flat_map(|turn| turn.items)
            .collect::<Vec<_>>(),
        saved
            .into_iter()
            .rev()
            .map(|entry| entry.item)
            .collect::<Vec<_>>()
    );
    Ok(())
}

async fn newest_items(
    mcp: &mut TestAppServer,
    thread_id: &str,
    limit: Option<u32>,
) -> Result<ThreadItemsListResponse> {
    mcp.request(|request_id| ClientRequest::ThreadItemsList {
        request_id,
        params: ThreadItemsListParams {
            defer_output: None,
            thread_id: thread_id.to_string(),
            turn_id: None,
            cursor: None,
            limit,
            sort_direction: Some(SortDirection::Desc),
        },
    })
    .await
}
