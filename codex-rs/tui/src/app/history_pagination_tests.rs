//! Older pages must preserve newer events and identify repeated hidden prompts precisely.

use super::*;
use crate::app::test_support::make_test_app;
use crate::history_cell::ComputerActivityCell;
use crate::history_cell::FinalMessageSeparator;
use crate::history_cell::ForkedThreadHistoryCell;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::UserInput;
use pretty_assertions::assert_eq;

fn turn(id: &str, status: TurnStatus, item_ids: &[&str]) -> Turn {
    Turn {
        id: id.to_string(),
        root_turn_id: None,
        items: item_ids
            .iter()
            .map(|id| ThreadItemPayload::UserMessage {
                source_response_item_id: None,
                id: id.to_string(),
                client_id: None,
                content: Vec::new(),
            })
            .map(ThreadItem::untimed)
            .collect(),
        items_view: TurnItemsView::Full,
        status,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    }
}

#[test]
fn overlapping_history_keeps_live_turn_state_and_newer_items() {
    let mut current = vec![turn("shared", TurnStatus::InProgress, &["overlap", "live"])];
    let older = vec![
        turn("old", TurnStatus::Completed, &["first"]),
        turn("shared", TurnStatus::Completed, &["before", "overlap"]),
    ];
    merge_older_turns(&mut current, older);
    assert_eq!(
        current,
        vec![
            turn("old", TurnStatus::Completed, &["first"]),
            turn(
                "shared",
                TurnStatus::InProgress,
                &["before", "overlap", "live"]
            ),
        ],
    );
}

fn user_cell(message: &str) -> Arc<dyn HistoryCell> {
    Arc::new(UserHistoryCell {
        timestamp: None,
        prompt_identity: Default::default(),
        presentation: Default::default(),
        message: message.to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    })
}

#[tokio::test]
async fn fork_lineage_stays_before_repeated_older_history_pages() {
    let mut app = make_test_app().await;
    app.transcript_cells = vec![
        Arc::new(ForkedThreadHistoryCell(
            crate::history_cell::PlainHistoryCell::new(vec!["Thread forked from parent".into()]),
        )),
        Arc::new(crate::history_cell::PlainHistoryCell::new(vec![
            "recent history".into(),
        ])),
    ];

    app.prepend_older_transcript_cells(vec![Arc::new(crate::history_cell::PlainHistoryCell::new(
        vec!["older history".into()],
    ))]);
    app.prepend_older_transcript_cells(vec![Arc::new(crate::history_cell::PlainHistoryCell::new(
        vec!["oldest history".into()],
    ))]);

    let rendered = app
        .transcript_cells
        .iter()
        .flat_map(|cell| cell.display_lines(/*width*/ 80))
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(rendered, @r###"
    Thread forked from parent
    oldest history
    older history
    recent history
    "###);
}

#[test]
fn repeated_prompt_matching_removes_only_the_hidden_occurrence() {
    let cells = vec![user_cell("repeat"), user_cell("other"), user_cell("repeat")];
    let persisted = vec![
        ("unloaded".to_string(), user_cell("repeat")),
        ("hidden".to_string(), user_cell("repeat")),
        ("middle".to_string(), user_cell("other")),
        ("visible".to_string(), user_cell("repeat")),
    ];
    assert_eq!(
        hidden_transcript_indices(&cells, persisted, &HashSet::from(["hidden", "unloaded"])),
        vec![0],
    );
}

#[tokio::test]
async fn resume_places_interruption_before_searches_split_across_pages() {
    let (mut app, mut events, _) = crate::app::tests::make_test_app_with_channels().await;
    let mut canceled = turn("canceled", TurnStatus::Interrupted, &[]);
    canceled.items_view = TurnItemsView::NotLoaded;
    let search = |query: &str| {
        serde_json::from_value(serde_json::json!({
            "type": "webSearch", "id": query, "query": query,
        }))
        .expect("valid search")
    };
    let mut audit = turn("audit", TurnStatus::Completed, &[]);
    audit.items = vec![search("newer")];
    app.chat_widget.replay_thread_turns(
        vec![canceled.clone(), audit.clone()],
        ReplayKind::ResumeInitialMessages,
    );
    let mut cells: Vec<Arc<dyn HistoryCell>> = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            cells.push(Arc::from(cell));
        }
    }

    canceled.items.push(search("canceled"));
    audit.items.insert(/*index*/ 0, search("older"));
    let page = vec![canceled.items[0].clone(), audit.items[0].clone()];
    let cwd = app.config.cwd.clone();
    let mut tui = crate::tui::test_support::make_test_tui().expect("test terminal");
    let older = app.project_older_history_cells(
        &mut tui,
        HistoryPage::Items(page),
        &[canceled, audit],
        &HashSet::new(),
        ThreadId::new(),
        &cwd,
        RawReasoningVisibility::Hidden,
    );
    cells.splice(0..0, older);
    let rows = cells
        .iter()
        .flat_map(|cell| cell.display_lines(/*width*/ 100))
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            "• Searched the web for canceled",
            "■ Conversation interrupted - use /feedback if something went wrong",
            "• Searched the web for older",
            "• Searched the web for newer",
        ]
    );
}

#[tokio::test]
async fn hidden_last_item_keeps_turn_groups_and_completion_boundaries() {
    let computer = |id: &str| {
        serde_json::from_value(serde_json::json!({
            "type": "mcpToolCall", "id": id, "server": "cua_repl", "tool": "js",
            "status": "completed", "arguments": {"title": format!("Inspect {id}")},
            "appContext": null, "pluginId": null, "readOnlyHint": null,
            "result": {"content": [], "structuredContent": null},
            "error": null, "durationMs": 10
        }))
        .expect("valid completed computer call")
    };
    for status in [
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Interrupted,
    ] {
        let mut app = make_test_app().await;
        let mut tui = crate::tui::test_support::make_test_tui().unwrap();
        let cwd = app.config.cwd.clone();
        let thread_id = ThreadId::new();
        let expected_completions = usize::from(status == TurnStatus::Completed);
        let mut first = turn("first", status, &[]);
        first.items = vec![
            ThreadItem::untimed(ThreadItemPayload::EnteredReviewMode {
                id: "enter-review".to_string(),
                review: "current changes".to_string(),
            }),
            computer("older-call"),
            ThreadItem::untimed(ThreadItemPayload::UserMessage {
                source_response_item_id: None,
                id: "hidden".to_string(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "internal review prompt".to_string(),
                    text_elements: Vec::new(),
                }],
            }),
        ];
        first.completed_at = Some(1_700_000_000);
        first.duration_ms = Some(125_000);
        let mut second = turn("second", TurnStatus::InProgress, &[]);
        second.items = vec![computer("newer-call")];
        let turns = vec![first, second];
        let hidden = hidden_review_item_ids(&turns);
        assert_eq!(hidden, HashSet::from(["hidden"]));
        let items = turns
            .iter()
            .flat_map(|turn| &turn.items)
            .cloned()
            .collect::<Vec<_>>();

        let cells = app.project_older_history_cells(
            &mut tui,
            HistoryPage::Items(items.clone()),
            &turns,
            &hidden,
            thread_id,
            &cwd,
            RawReasoningVisibility::Hidden,
        );
        let groups = cells
            .iter()
            .filter_map(|cell| cell.as_any().downcast_ref::<ComputerActivityCell>())
            .map(|cell| cell.call_ids().map(str::to_string).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let completions = cells
            .iter()
            .filter(|cell| cell.as_any().is::<FinalMessageSeparator>())
            .count();
        let visible_users = cells
            .iter()
            .filter(|cell| cell.as_any().is::<UserHistoryCell>())
            .count();
        assert_eq!(
            (groups, completions, visible_users),
            (
                vec![
                    vec!["older-call".to_string()],
                    vec!["newer-call".to_string()]
                ],
                expected_completions,
                0,
            ),
        );

        let repeated = app.project_older_history_cells(
            &mut tui,
            HistoryPage::Items(items),
            &turns,
            &hidden,
            thread_id,
            &cwd,
            RawReasoningVisibility::Hidden,
        );
        assert_eq!(
            repeated
                .iter()
                .filter(|cell| cell.as_any().is::<FinalMessageSeparator>())
                .count(),
            0,
        );
    }
}
