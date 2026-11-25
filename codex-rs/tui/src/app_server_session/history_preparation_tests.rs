use super::*;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::TurnStatus;
use pretty_assertions::assert_eq;
use std::future::ready;

fn item(turn_id: &str, id: &str) -> ThreadItemEntry {
    ThreadItemEntry {
        turn_id: turn_id.to_owned(),
        item: ThreadItem::untimed(ThreadItemPayload::EnteredReviewMode {
            id: id.to_owned(),
            review: turn_id.to_owned(),
        }),
        output_deferred: false,
        started_at_ms: Some(100),
        completed_at_ms: Some(200),
    }
}

fn metadata(id: &str) -> Turn {
    Turn {
        id: id.to_owned(),
        root_turn_id: None,
        items: Vec::new(),
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
    let mut state = ThreadHistoryPagination {
        next_turn_cursor: Some("overlap".to_owned()),
        next_item_cursor: Some("newest-items".to_owned()),
        ..ThreadHistoryPagination::default()
    };
    let item_page = |data, next: &str| ThreadItemsListResponse {
        data,
        next_cursor: Some(next.to_owned()),
        backwards_cursor: None,
    };
    let first = item_page(
        vec![item("recent", "shared"), item("old", "shared")],
        "older-items",
    );
    let turn_page = |id, next: Option<&str>| {
        Ok(ThreadTurnsListResponse {
            data: vec![metadata(id)],
            next_cursor: next.map(str::to_owned),
            backwards_cursor: None,
        })
    };
    let mut fetch_turns = |cursor: String, limit| {
        assert_eq!(limit, 1);
        ready(match cursor.as_str() {
            "overlap" => turn_page("recent", Some("gap")),
            "gap" => turn_page("empty", Some("old")),
            "old" => Err(eyre!("request interrupted")),
            _ => Err(eyre!("unexpected metadata cursor {cursor}")),
        })
    };
    let mut turns = vec![metadata("recent")];
    let mut admitted = Vec::new();
    let mut admit = |turn_id: &str, item: &ThreadItem| {
        admitted.push((turn_id.to_owned(), item.id().to_owned()));
    };
    let result = HistoryBatch::new(&mut turns)
        .merge_items(first.clone(), &mut state, &mut fetch_turns, &mut admit)
        .await;
    assert!(result.is_err());
    assert_eq!(state.next_item_cursor.as_deref(), Some("newest-items"));
    assert_eq!(turns[0].id, "empty");
    assert_eq!(turns[1].items[0].id(), "shared");

    {
        let mut batch = HistoryBatch::new(&mut turns);
        let mut fetch_turns = |cursor, limit| {
            assert_eq!((cursor, limit), ("old".to_owned(), 1));
            ready(turn_page("old", None))
        };
        batch
            .merge_items(first, &mut state, &mut fetch_turns, &mut admit)
            .await?;
        assert_eq!(state.next_item_cursor.as_deref(), Some("older-items"));
        let older = item_page(
            vec![
                // Repeated IDs in this turn neither replace the first payload nor retry conversion.
                ThreadItemEntry {
                    output_deferred: true,
                    ..item("old", "shared")
                },
                item("old", "earlier"),
            ],
            "newest-items",
        );
        batch
            .merge_items(older, &mut state, &mut fetch_turns, &mut admit)
            .await?;
    }
    assert_eq!(state.next_item_cursor, None);
    assert_eq!(state.next_turn_cursor, None);
    assert_eq!(
        admitted
            .iter()
            .map(|(turn, item)| (turn.as_str(), item.as_str()))
            .collect::<Vec<_>>(),
        [("recent", "shared"), ("old", "shared"), ("old", "earlier")]
    );
    assert_eq!(
        turns
            .iter()
            .map(|turn| {
                (
                    turn.id.as_str(),
                    turn.items.iter().map(ThreadItem::id).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
        [
            ("old", vec!["earlier", "shared"]),
            ("empty", vec![]),
            ("recent", vec!["shared"])
        ]
    );
    Ok(())
}
