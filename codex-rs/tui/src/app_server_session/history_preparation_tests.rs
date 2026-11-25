use super::*;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::TurnStatus;
use pretty_assertions::assert_eq;
use std::future::ready;

fn item(turn_id: &str, id: &str, text: &str) -> ThreadItemEntry {
    ThreadItemEntry {
        turn_id: turn_id.to_owned(),
        item: ThreadItem::untimed(ThreadItemPayload::EnteredReviewMode {
            id: id.to_owned(),
            review: text.to_owned(),
        }),
        output_deferred: false,
        started_at_ms: Some(100),
        completed_at_ms: Some(200),
    }
}

fn metadata(id: &str, items: Vec<ThreadItem>) -> Turn {
    Turn {
        id: id.to_owned(),
        root_turn_id: None,
        items,
        items_view: TurnItemsView::NotLoaded,
        status: TurnStatus::Completed,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    }
}

#[tokio::test]
async fn history_pages_keep_turn_scoped_ids_and_retry_after_partial_admission() -> Result<()> {
    let newest = ThreadItem::try_from(item("recent", "tail", "preloaded"))?;
    let recent = item("recent", "shared", "recent");
    let old = item("old", "shared", "old");
    let earlier = item("old", "earlier", "earlier");
    let newest_page = ThreadItemsListResponse {
        data: vec![recent.clone(), old.clone()],
        next_cursor: Some("older-items".to_owned()),
        backwards_cursor: None,
    };
    let older_items = ThreadItemsListResponse {
        data: vec![
            // The overlap must not displace the first item or try to load it again.
            ThreadItemEntry {
                output_deferred: true,
                ..item("old", "shared", "overlap")
            },
            earlier.clone(),
        ],
        next_cursor: Some("newest-items".to_owned()),
        backwards_cursor: None,
    };
    for hydrating in [true, false] {
        let mut turns = vec![metadata("recent", vec![newest.clone()])];
        let mut state = ThreadHistoryPagination {
            next_turn_cursor: Some("recent-turns".to_owned()),
            next_item_cursor: Some("newest-items".to_owned()),
            ..ThreadHistoryPagination::default()
        };
        let mut metadata_pages = [
            Ok(ThreadTurnsListResponse {
                data: vec![
                    metadata("recent", Vec::new()),
                    metadata("empty", Vec::new()),
                ],
                next_cursor: Some("older-turns".to_owned()),
                backwards_cursor: None,
            }),
            Err(eyre!("request interrupted")),
            Ok(ThreadTurnsListResponse {
                data: vec![metadata("old", Vec::new())],
                next_cursor: None,
                backwards_cursor: None,
            }),
        ]
        .into_iter();
        let mut requests = Vec::new();
        let mut fetch_turns = |cursor, limit| {
            requests.push((cursor, limit));
            ready(
                metadata_pages
                    .next()
                    .unwrap_or_else(|| Err(eyre!("unexpected metadata fetch"))),
            )
        };
        let mut admitted = Vec::new();
        {
            let mut turns = if hydrating {
                HistoryTurns::hydrating(&mut turns)
            } else {
                HistoryTurns::chronological(&mut turns)
            };
            let mut admit = |turn_id: &str, item: &ThreadItem| {
                admitted.push((turn_id.to_owned(), item.clone()));
            };
            let result = turns
                .merge_items(
                    newest_page.clone(),
                    &mut state,
                    &mut fetch_turns,
                    &mut admit,
                )
                .await;
            assert!(result.is_err());
            assert_eq!(state.next_item_cursor.as_deref(), Some("newest-items"));
            turns
                .merge_items(
                    newest_page.clone(),
                    &mut state,
                    &mut fetch_turns,
                    &mut admit,
                )
                .await?;
            assert_eq!(state.next_item_cursor.as_deref(), Some("older-items"));
            turns
                .merge_items(
                    older_items.clone(),
                    &mut state,
                    &mut fetch_turns,
                    &mut admit,
                )
                .await?;
        }
        assert_eq!(state.next_item_cursor, None);
        assert_eq!(state.next_turn_cursor, None);
        assert_eq!(
            requests,
            [
                ("recent-turns".to_owned(), 1),
                ("older-turns".to_owned(), 1),
                ("older-turns".to_owned(), 1),
            ]
        );
        assert_eq!(
            admitted,
            [
                ("recent".to_owned(), ThreadItem::try_from(recent.clone())?),
                ("old".to_owned(), ThreadItem::try_from(old.clone())?),
                ("old".to_owned(), ThreadItem::try_from(earlier.clone())?),
            ]
        );
        assert_eq!(
            turns,
            [
                Turn {
                    items_view: TurnItemsView::Summary,
                    ..metadata(
                        "old",
                        vec![
                            ThreadItem::try_from(earlier.clone())?,
                            ThreadItem::try_from(old.clone())?,
                        ],
                    )
                },
                metadata("empty", Vec::new()),
                Turn {
                    items_view: TurnItemsView::Summary,
                    ..metadata(
                        "recent",
                        vec![ThreadItem::try_from(recent.clone())?, newest.clone()],
                    )
                },
            ]
        );
    }
    Ok(())
}
