//! Page transport cannot change activity chronology, rich details, or pending live completion.

use super::*;
use crate::app::test_support::make_test_app;
use crate::chatwidget::CompletedItem;
use crate::thread_transcript::RawReasoningVisibility;
use crate::thread_transcript::thread_items_to_transcript_cells;
use codex_app_server_protocol::TurnItemsView;
use pretty_assertions::assert_eq;

#[derive(Clone, Copy, Debug)]
enum ActivityKind {
    Computer,
    Exploration,
}

fn call(id: &str, kind: ActivityKind, status: &str) -> ThreadItem {
    let value = match kind {
        ActivityKind::Computer => serde_json::json!({
            "type": "mcpToolCall", "id": id, "server": "cua_repl", "tool": "js",
            "status": status, "arguments": {"title": format!("Inspect {id}")},
            "result": {"content": [{"type": "text", "text": format!("Result {id}")}]},
            "durationMs": 10
        }),
        ActivityKind::Exploration => serde_json::json!({
            "type": "commandExecution", "id": id, "command": format!("cat {id}.rs"),
            "cwd": crate::test_support::test_path_buf("/workspace"), "source": "agent",
            "status": status, "commandActions": [{"type": "read", "command": format!("cat {id}.rs"), "name": format!("{id}.rs"), "path": format!("{id}.rs")}],
            "aggregatedOutput": format!("Result {id}"), "exitCode": 0, "durationMs": 10
        }),
    };
    serde_json::from_value(value).expect("activity fixture")
}

fn reasoning(id: &str) -> ThreadItem {
    ThreadItem::Reasoning {
        id: id.to_owned(),
        summary: vec![format!(
            "**Considering {id}**\n\nRetained reasoning for {id}."
        )],
        content: Vec::new(),
    }
}

fn turn(items: Vec<ThreadItem>) -> Turn {
    Turn {
        id: "turn".to_owned(),
        items,
        items_view: TurnItemsView::Full,
        status: TurnStatus::Completed,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    }
}

fn project(app: &App, items: &[ThreadItem]) -> Vec<Arc<dyn HistoryCell>> {
    thread_items_to_transcript_cells(
        /*thread_id*/ None,
        &app.config.cwd,
        items.iter().cloned(),
        RawReasoningVisibility::Hidden,
        Some(&app.config),
    )
}

#[tokio::test]
async fn returned_batches_keep_one_disclosure_and_bill_across_live_replay_and_pages() {
    use crate::chatwidget::ReplayKind;
    use codex_protocol::models::FunctionCallOutputBody;

    let output = |id: &str| ThreadItem::FunctionCallOutput {
        id: id.to_owned(),
        call_id: Some(if id == "b" { "a" } else { id }.to_owned()),
        name: "exec".into(),
        namespace: None,
        output: FunctionCallOutputBody::Text(format!("Returned {id}\nOnly model-visible text.")),
    };
    let rendered = |cells: &[Arc<dyn HistoryCell>]| {
        cells
            .iter()
            .map(|cell| {
                (
                    cell.compact_hyperlink_lines(80),
                    cell.expanded_hyperlink_lines(80),
                    cell.activity_ids(),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut snapshots = Vec::new();
    for (kind, ordinary) in [
        (ActivityKind::Exploration, false),
        (ActivityKind::Computer, false),
        (ActivityKind::Exploration, true),
    ] {
        let (mut app, mut rx, _) = crate::app::tests::make_test_app_with_channels().await;
        let mut tui = crate::tui::test_support::make_test_tui().unwrap();
        tui.set_owned_screen(/*owned*/ true).unwrap();
        let command = |id| {
            let mut item = call(id, kind, "completed");
            if ordinary
                && let ThreadItem::CommandExecution {
                    command,
                    command_actions,
                    ..
                } = &mut item
            {
                *command = "cargo check".to_owned();
                command_actions.clear();
            }
            item
        };
        let current = turn(vec![
            command("first"),
            command("nested"),
            output("a"),
            reasoning("between-outputs"),
            output("b"),
            command("second"),
            output("c"),
        ]);
        let expected = project(&app, &current.items);
        assert_eq!(expected.len(), 2);

        // Nested calls and notify() outputs share a call; a different call starts a new unit.
        for item in &current.items {
            app.chat_widget.replay_thread_item(
                item.clone(),
                current.id.clone(),
                ReplayKind::ResumeInitialMessages,
            );
            while let Ok(event) = rx.try_recv() {
                if let AppEvent::InsertHistoryCell(cell) = event {
                    app.insert_history_cell(&mut tui, cell);
                }
            }
        }
        let separator: Arc<dyn HistoryCell> =
            Arc::new(history_cell::PlainHistoryCell::new(vec!["Answer".into()]));
        app.chat_widget
            .add_to_history(history_cell::PlainHistoryCell::new(vec!["Answer".into()]));
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                app.insert_history_cell(&mut tui, cell);
            }
        }
        assert_eq!(
            rendered(&app.transcript_cells[..app.transcript_cells.len() - 1]),
            rendered(&expected),
            "live {kind:?} ordinary={ordinary}"
        );

        for split in 1..current.items.len() {
            let older = project(&app, &current.items[..split]);
            let boundary = older.len();
            app.transcript_cells = older
                .into_iter()
                .chain(project(&app, &current.items[split..]))
                .chain([Arc::clone(&separator)])
                .collect();
            app.join_older_activity_group(boundary, std::slice::from_ref(&current));
            assert_eq!(
                rendered(&app.transcript_cells[..app.transcript_cells.len() - 1]),
                rendered(&expected),
                "page {kind:?} ordinary={ordinary} split={split}"
            );
        }
        for (id, count) in [("a", 1000), ("b", 280), ("c", 20), ("a", 1000)] {
            app.transcript_view.record_tool_output_tokens(
                format!("tool-output-tokens:{id}"),
                if id == "b" { "a" } else { id }.into(),
                Some(count),
            );
        }
        // Exercise production rendering before checking the persisted batch totals.
        let screen_size = tui.terminal.size().unwrap();
        app.render_owned_transcript(&mut tui, screen_size).unwrap();
        assert_eq!(
            expected
                .iter()
                .map(|cell| app
                    .transcript_view
                    .activity_output_tokens(&cell.activity_ids()))
                .collect::<Vec<_>>(),
            vec![Some(1280), Some(20)]
        );
        for expanded in [false, true] {
            if expanded {
                app.transcript_view
                    .handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE), &expected);
                for _ in &expected {
                    app.transcript_view
                        .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &expected);
                    app.transcript_view
                        .handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &expected);
                }
                app.transcript_view.clear_activity_focus();
            }
            app.transcript_view.jump_to_latest();
            let area = Rect::new(
                /*x*/ 0, /*y*/ 0, /*width*/ 100, /*height*/ 32,
            );
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            app.transcript_view.render(area, &mut buffer, &expected);
            let lines = (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n");
            snapshots.push(format!(
                "{kind:?} ordinary={ordinary} expanded={expanded}\n{}",
                lines.trim()
            ));
        }
    }
    insta::assert_snapshot!("coalesced_returned_batches", snapshots.join("\n\n"));
}

#[tokio::test]
async fn patch_files_keep_disclosures_and_one_batch_bill_across_live_replay_and_pages() {
    use crate::chatwidget::ReplayKind;
    use codex_app_server_protocol::FileUpdateChange;
    use codex_app_server_protocol::PatchApplyStatus;
    use codex_app_server_protocol::PatchChangeKind;
    use codex_protocol::models::FunctionCallOutputBody;

    // Batch output must enrich Edited without absorbing its independently disclosed files.
    let output = |id: &str| ThreadItem::FunctionCallOutput {
        id: id.into(),
        call_id: Some(id.into()),
        name: "exec".into(),
        namespace: None,
        output: FunctionCallOutputBody::Text(format!("Returned {id}")),
    };
    let current = turn(vec![
        ThreadItem::FileChange {
            id: "patch".into(),
            changes: ["alpha.rs", "beta.rs"]
                .into_iter()
                .map(|path| FileUpdateChange {
                    path: path.into(),
                    kind: PatchChangeKind::Add,
                    diff: format!("{path} body\n"),
                })
                .collect(),
            status: PatchApplyStatus::Completed,
        },
        output("a"),
        reasoning("between-outputs"),
        output("b"),
    ]);
    let rendered = |cells: &[Arc<dyn HistoryCell>]| {
        cells
            .iter()
            .map(|cell| {
                (
                    cell.activity_ids(),
                    cell.compact_hyperlink_lines(/*width*/ 80),
                    cell.expanded_hyperlink_lines(/*width*/ 80),
                )
            })
            .collect::<Vec<_>>()
    };
    for (file_count, with_command) in [(2, false), (2, true), (1, true)] {
        let mut current = current.clone();
        if let ThreadItem::FileChange { changes, .. } = &mut current.items[0] {
            changes.truncate(file_count);
        }
        if with_command {
            let mut command = call("command", ActivityKind::Exploration, "completed");
            if let ThreadItem::CommandExecution {
                command,
                command_actions,
                ..
            } = &mut command
            {
                *command = "cargo check".into();
                command_actions.clear();
            }
            current.items.insert(/*index*/ 1, command);
        }
        let (mut app, mut rx, _) = crate::app::tests::make_test_app_with_channels().await;
        let mut tui = crate::tui::test_support::make_test_tui().unwrap();
        tui.set_owned_screen(/*owned*/ true).unwrap();
        let expected = project(&app, &current.items);
        assert_eq!(expected.len(), file_count + usize::from(with_command) + 2);
        for item in &current.items {
            app.chat_widget.replay_thread_item(
                item.clone(),
                current.id.clone(),
                ReplayKind::ResumeInitialMessages,
            );
            while let Ok(event) = rx.try_recv() {
                if let AppEvent::InsertHistoryCell(cell) = event {
                    app.insert_history_cell(&mut tui, cell);
                }
            }
        }
        assert_eq!(rendered(&app.transcript_cells), rendered(&expected));
        for split in 1..current.items.len() {
            let older = project(&app, &current.items[..split]);
            let boundary = older.len();
            app.transcript_cells = older
                .into_iter()
                .chain(project(&app, &current.items[split..]))
                .collect();
            app.join_older_activity_group(boundary, std::slice::from_ref(&current));
            assert_eq!(
                rendered(&app.transcript_cells),
                rendered(&expected),
                "file_count={file_count} with_command={with_command} split={split}"
            );
        }
        for (id, tokens) in [("a", 100), ("b", 23), ("a", 100)] {
            app.transcript_view.record_tool_output_tokens(
                format!("tool-output-tokens:{id}"),
                "outer-exec".into(),
                Some(tokens),
            );
        }
        assert_eq!(
            expected
                .iter()
                .map(|cell| app
                    .transcript_view
                    .activity_output_tokens(&cell.activity_ids()))
                .collect::<Vec<_>>(),
            (0..expected.len())
                .map(|index| {
                    if index == if with_command { file_count } else { 0 } {
                        Some(100)
                    } else if index == expected.len() - 1 {
                        Some(23)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn older_page_hydration_keeps_pending_computer_and_exploration_completion() {
    for kind in [ActivityKind::Computer, ActivityKind::Exploration] {
        let mut app = make_test_app().await;
        match kind {
            ActivityKind::Computer => {
                app.chat_widget
                    .handle_mcp_tool_call_completed_now(CompletedItem {
                        item: call("newer", kind, "completed"),
                        timestamp: None,
                    });
                app.chat_widget.handle_mcp_tool_call_started_now(call(
                    "pending",
                    kind,
                    "inProgress",
                ));
            }
            ActivityKind::Exploration => {
                app.chat_widget
                    .handle_command_execution_completed_now(CompletedItem {
                        item: call("newer", kind, "completed"),
                        timestamp: None,
                    });
                app.chat_widget.handle_command_execution_started_now(call(
                    "pending",
                    kind,
                    "inProgress",
                ));
            }
        }
        let mut current = turn(vec![
            call("older", kind, "completed"),
            reasoning("boundary"),
            call("newer", kind, "completed"),
            call("pending", kind, "inProgress"),
        ]);
        current.status = TurnStatus::InProgress;
        // Separate projections model reasoning arriving on its own intervening page.
        app.transcript_cells = project(&app, &current.items[..1]);
        app.transcript_cells
            .extend(project(&app, &current.items[1..2]));
        app.join_older_activity_group(/*boundary*/ 1, std::slice::from_ref(&current));
        assert_eq!(
            app.transcript_cells.len(),
            if matches!(kind, ActivityKind::Computer) {
                0
            } else {
                2
            }
        );
        if matches!(kind, ActivityKind::Computer) {
            assert!(
                crate::chatwidget::tests::helpers::render_bottom_popup(
                    &app.chat_widget,
                    /*width*/ 80
                )
                .contains("Using computer · 3 actions")
            );
        }
        match kind {
            ActivityKind::Computer => {
                app.chat_widget
                    .handle_mcp_tool_call_completed_now(CompletedItem {
                        item: call("pending", kind, "completed"),
                        timestamp: None,
                    })
            }
            ActivityKind::Exploration => {
                app.chat_widget
                    .handle_command_execution_completed_now(CompletedItem {
                        item: call("pending", kind, "completed"),
                        timestamp: None,
                    })
            }
        }
        current.items[3] = call("pending", kind, "completed");
        let expected = project(&app, &current.items);
        match kind {
            ActivityKind::Computer => assert_eq!(
                app.chat_widget
                    .active_cell_transcript_lines(/*width*/ 80)
                    .unwrap(),
                expected[0].transcript_lines(/*width*/ 80),
            ),
            ActivityKind::Exploration => {
                assert!(
                    app.chat_widget
                        .active_cell_transcript_lines(/*width*/ 80)
                        .is_none()
                );
                assert_eq!(expected.len(), 4);
            }
        }
    }
}

#[tokio::test]
async fn trailing_reasoning_identity_rejects_identical_text_from_another_turn() {
    for kind in [ActivityKind::Computer, ActivityKind::Exploration] {
        let mut app = make_test_app().await;
        let mut same_turn = reasoning("same-turn-detail");
        let mut next_turn = reasoning("next-turn-detail");
        for item in [&mut same_turn, &mut next_turn] {
            if let ThreadItem::Reasoning { summary, .. } = item {
                *summary = vec!["Exactly the same reasoning text".to_owned()];
            }
        }
        let first = turn(vec![call("call", kind, "completed"), same_turn.clone()]);
        let mut second = turn(vec![next_turn.clone()]);
        second.id = "next-turn".to_owned();
        let turns = vec![first, second];
        for detail in [same_turn, next_turn] {
            let older = project(&app, &turns[0].items[..1]).remove(/*index*/ 0);
            let trailing = project(&app, std::slice::from_ref(&detail)).remove(/*index*/ 0);
            app.transcript_cells = vec![Arc::clone(&older), Arc::clone(&trailing)];
            app.join_older_activity_group(/*boundary*/ 1, &turns);
            if detail.id() == "same-turn-detail" && matches!(kind, ActivityKind::Computer) {
                let expected = project(&app, &turns[0].items);
                assert_eq!(app.transcript_cells.len(), 1);
                assert_eq!(app.transcript_cells[0].raw_lines(), expected[0].raw_lines());
            } else {
                assert_eq!(app.transcript_cells.len(), 2);
                assert!(Arc::ptr_eq(&app.transcript_cells[0], &older));
                assert!(Arc::ptr_eq(&app.transcript_cells[1], &trailing));
            }
        }
        // Live reasoning without persisted identity cannot prove an older-page relationship.
        let unidentified: Arc<dyn HistoryCell> = Arc::new(history_cell::ReasoningSummaryCell::new(
            String::new(),
            "Exactly the same reasoning text".to_owned(),
            app.config.cwd.as_path(),
            /*transcript_only*/ true,
        ));
        app.transcript_cells = project(&app, &turns[0].items[..1]);
        app.transcript_cells.push(Arc::clone(&unidentified));
        app.join_older_activity_group(/*boundary*/ 1, &turns);
        assert_eq!(app.transcript_cells.len(), 2);
        assert!(Arc::ptr_eq(&app.transcript_cells[1], &unidentified));
    }
}

#[tokio::test]
async fn every_page_split_folds_reasoning_before_answer_and_completion_boundaries() {
    for kind in [ActivityKind::Computer, ActivityKind::Exploration] {
        for answer in [None, Some("The answer")] {
            let mut app = make_test_app().await;
            let mut items = vec![call("call", kind, "completed"), reasoning("detail")];
            if let Some(text) = answer {
                items.push(ThreadItem::AgentMessage {
                    id: "answer".to_owned(),
                    text: text.to_owned(),
                    phase: None,
                    memory_citation: None,
                    delivery: None,
                    questions: None,
                });
            }
            let current = turn(items);
            let separator: Arc<dyn HistoryCell> =
                Arc::new(history_cell::FinalMessageSeparator::new(
                    /*elapsed_seconds*/ Some(65),
                    /*runtime_metrics*/ None,
                ));
            let mut expected = project(&app, &current.items);
            expected.push(Arc::clone(&separator));
            for split in 1..current.items.len() {
                let older = project(&app, &current.items[..split]);
                let mut newer = project(&app, &current.items[split..]);
                newer.push(Arc::clone(&separator));
                let boundary = older.len();
                app.transcript_cells = older.into_iter().chain(newer).collect();
                app.join_older_activity_group(boundary, std::slice::from_ref(&current));
                for width in [24, 80] {
                    let rendered = |cells: &[Arc<dyn HistoryCell>]| {
                        cells
                            .iter()
                            .map(|cell| {
                                (
                                    cell.display_hyperlink_lines(width),
                                    cell.transcript_hyperlink_lines(width),
                                    cell.raw_lines(),
                                )
                            })
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(
                        rendered(&app.transcript_cells),
                        rendered(&expected),
                        "{kind:?}, answer {answer:?}, split {split}"
                    );
                }
                assert!(Arc::ptr_eq(
                    app.transcript_cells.last().unwrap(),
                    &separator
                ));
            }
        }
    }
}

#[tokio::test]
async fn failed_exploration_retains_following_reasoning_at_every_page_split() {
    let mut app = make_test_app().await;
    let mut failed = call("failed", ActivityKind::Exploration, "failed");
    if let ThreadItem::CommandExecution { exit_code, .. } = &mut failed {
        *exit_code = Some(1);
    }
    let current = turn(vec![
        call("first", ActivityKind::Exploration, "completed"),
        failed,
        reasoning("after-failure"),
    ]);
    let expected = project(&app, &current.items);
    assert_eq!(expected.len(), 3);
    for split in 1..current.items.len() {
        let older = project(&app, &current.items[..split]);
        let boundary = older.len();
        app.transcript_cells = older
            .into_iter()
            .chain(project(&app, &current.items[split..]))
            .collect();
        app.join_older_activity_group(boundary, std::slice::from_ref(&current));
        let rendered = |cells: &[Arc<dyn HistoryCell>]| {
            cells
                .iter()
                .map(|cell| cell.transcript_hyperlink_lines(/*width*/ 80))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            rendered(&app.transcript_cells),
            rendered(&expected),
            "split {split}"
        );
    }
}
