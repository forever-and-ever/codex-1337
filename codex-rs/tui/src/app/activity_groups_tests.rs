//! Page transport cannot change activity chronology, rich details, or pending live completion.

use super::*;
use crate::app::test_support::make_test_app;
use crate::history_cell::activity_parts::FoldState;
use crate::thread_transcript::RawReasoningVisibility;
use crate::thread_transcript::thread_items_to_transcript_cells;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::TurnItemsView;
use pretty_assertions::assert_eq;

#[derive(Clone, Copy, Debug)]
enum ActivityKind {
    Computer,
    Exploration,
}

fn call(id: &str, kind: ActivityKind, status: &str) -> ThreadItemPayload {
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

fn reasoning(id: &str) -> ThreadItemPayload {
    ThreadItemPayload::Reasoning {
        id: id.to_owned(),
        summary: vec![format!(
            "**Considering {id}**\n\nRetained reasoning for {id}."
        )],
        content: Vec::new(),
    }
}

fn turn(items: Vec<ThreadItemPayload>) -> Turn {
    Turn {
        id: "turn".to_owned(),
        root_turn_id: None,
        items: items.into_iter().map(ThreadItem::untimed).collect(),
        items_view: TurnItemsView::Full,
        status: TurnStatus::Completed,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    }
}

fn code_mode(exec: &str, event: codex_app_server_protocol::CodeModeEvent) -> ThreadItemPayload {
    ThreadItemPayload::CodeMode(codex_app_server_protocol::CodeModeItem {
        id: format!("fact:{exec}:{event:?}"),
        cell_id: Some("reused-runtime-cell".into()),
        exec_call_id: exec.into(),
        created_at: 0,
        event,
    })
}

#[tokio::test]
async fn scoped_code_mode_admission_survives_interleaving_and_page_prefixes() {
    use crate::code_mode_transcript::Admission;
    use crate::code_mode_transcript::Reducers;
    use crate::code_mode_transcript::Source;
    use codex_app_server_protocol::CodeModeEvent;
    use codex_app_server_protocol::CodeModeOwned;
    use codex_app_server_protocol::CodeModeOwner;
    use codex_app_server_protocol::CodeModeStatus;
    use codex_app_server_protocol::ItemCompletedNotification;
    use codex_app_server_protocol::ItemStartedNotification;
    let thread = ThreadId::from_string("11111111-1111-4111-8111-111111111111").unwrap();
    let fork = ThreadId::from_string("22222222-2222-4222-8222-222222222222").unwrap();
    let sibling = ThreadId::from_string("33333333-3333-4333-8333-333333333333").unwrap();
    let owner = |exec: &str| CodeModeOwner {
        thread_id: thread,
        exec_call_id: exec.into(),
        cell_id: None,
        started_at_ms: 0,
    };
    let owned_item = |owner: &CodeModeOwner, mut item: ThreadItemPayload| {
        if let ThreadItemPayload::CodeMode(fact) = &mut item {
            fact.cell_id = owner.cell_id.clone();
        }
        ThreadItemPayload::CodeModeOwned(CodeModeOwned::new(owner.clone(), Box::new(item)))
    };
    let notification = |owner: &CodeModeOwner, item: ThreadItemPayload| {
        let ThreadItemPayload::CodeModeOwned(owned) = owned_item(owner, item) else {
            unreachable!()
        };
        ServerNotification::ItemCompleted(ItemCompletedNotification::new(
            thread.to_string(),
            "turn".into(),
            *owned.into_parts().1,
            None,
            0,
        ))
        .with_code_mode_owner(owner)
    };
    let output = |id: &str, call: &str, text: &str| ThreadItemPayload::FunctionCallOutput {
        id: id.into(),
        call_id: Some(call.into()),
        name: "exec".into(),
        namespace: None,
        output: codex_protocol::models::FunctionCallOutputBody::Text(text.into()),
    };
    let (mut app, mut rx, _) = crate::app::tests::make_test_app_with_channels().await;
    app.local_settings.tui.animations = false;
    app.chat_widget
        .handle_thread_session(crate::app::tests::test_thread_session(
            thread,
            app.config.cwd.to_path_buf(),
        ));
    while rx.try_recv().is_ok() {}
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    tui.set_owned_screen(/*owned*/ true).unwrap();
    let drain = |app: &mut App,
                 rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
                 tui: &mut crate::tui::Tui| {
        let mut order = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::InsertHistoryCell(cell) => app.insert_history_cell(tui, cell),
                AppEvent::ConsolidateAgentMessage {
                    deferred_history_cell,
                    ..
                } => {
                    order.push("preceding stream");
                    if let Some(cell) = deferred_history_cell {
                        app.insert_history_cell(tui, cell);
                    }
                }
                AppEvent::CodeModeProjection { projection, .. } => {
                    order.push("owned projection");
                    app.apply_code_mode_projection(tui, projection);
                }
                AppEvent::CodeModeTranscript {
                    thread_id,
                    admission,
                } => app.apply_code_mode_admission(tui, thread_id, admission),
                _ => {}
            }
        }
        order
    };
    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        ServerNotification::AgentMessageDelta(
            codex_app_server_protocol::AgentMessageDeltaNotification {
                thread_id: thread.to_string(),
                turn_id: "turn".into(),
                item_id: "before".into(),
                delta: "Before script".into(),
            },
        ),
    )));
    let a = owner("exec-a");
    let b = owner("exec-b");
    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(notification(
        &a,
        code_mode(
            "exec-a",
            CodeModeEvent::Started {
                call_id: "exec-a".into(),
                script: None,
            },
        ),
    ))));
    let order = drain(&mut app, &mut rx, &mut tui);
    assert!(
        order
            .iter()
            .position(|event| *event == "preceding stream")
            .unwrap()
            < order
                .iter()
                .position(|event| *event == "owned projection")
                .unwrap()
    );
    for (owner, item) in [
        (
            &b,
            code_mode(
                "exec-b",
                CodeModeEvent::Started {
                    call_id: "exec-b".into(),
                    script: None,
                },
            ),
        ),
        (&a, {
            let mut item = call("child-a", ActivityKind::Exploration, "inProgress");
            if let ThreadItemPayload::CommandExecution { process_id, .. } = &mut item {
                *process_id = Some("proc-a".into());
            }
            item
        }),
        (&b, call("child-b", ActivityKind::Computer, "inProgress")),
    ] {
        app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(notification(
            owner, item,
        ))));
        drain(&mut app, &mut rx, &mut tui);
        assert_eq!(
            app.transcript_cells
                .iter()
                .filter(|cell| cell.as_any().is::<history_cell::CodeModeCell>())
                .count(),
            2
        );
    }
    for stdin in ["hello\n", ""] {
        app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
            ServerNotification::TerminalInteraction(
                codex_app_server_protocol::TerminalInteractionNotification {
                    thread_id: thread.to_string(),
                    turn_id: "turn".into(),
                    item_id: "child-a".into(),
                    process_id: "proc-a".into(),
                    stdin: stdin.into(),
                },
            )
            .with_code_mode_owner(&a),
        )));
        drain(&mut app, &mut rx, &mut tui);
    }
    let render = |view: &mut crate::transcript_view::TranscriptView,
                  cells: &[Arc<dyn HistoryCell>]| {
        let area = ratatui::layout::Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 90, /*height*/ 12,
        );
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        view.render(area, &mut buffer, cells);
        buffer
            .content()
            .chunks(90)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned()
    };
    render(&mut app.transcript_view, &app.transcript_cells);
    let a_index = app
        .transcript_cells
        .iter()
        .position(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::CodeModeCell>()
                .is_some_and(|block| block.exec_call_id() == "exec-a")
        })
        .unwrap();
    app.transcript_view
        .jump_to_entry(&app.transcript_cells, a_index);
    let held = render(&mut app.transcript_view, &app.transcript_cells);
    // A JS result is not a process exit: its background command keeps its own lease.
    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(notification(
        &a,
        code_mode(
            "exec-a",
            CodeModeEvent::Finished {
                status: CodeModeStatus::Completed,
            },
        ),
    ))));
    drain(&mut app, &mut rx, &mut tui);
    let background = app
        .transcript_cells
        .iter()
        .find_map(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::CodeModeCell>()
                .filter(|cell| cell.owner() == Some(&a))
        })
        .unwrap();
    assert!(background.terminal());
    assert!(background.is_active());

    for item in [
        call("child-a", ActivityKind::Exploration, "completed"),
        code_mode(
            "exec-a",
            CodeModeEvent::WaitStarted {
                call_id: "wait-a".into(),
                target: None,
            },
        ),
        code_mode(
            "exec-a",
            CodeModeEvent::WaitCompleted {
                call_id: "wait-a".into(),
                target: None,
            },
        ),
        output("notify-a", "exec-a", "{\"answer\":42}"),
        output("returned-a", "exec-a", "Exact accepted return"),
        ThreadItemPayload::ToolOutputTokens {
            id: "tool-output-tokens:notify-a".into(),
            call_id: "exec-a".into(),
            tokens: Some(3),
        },
        ThreadItemPayload::ToolOutputTokens {
            id: "tool-output-tokens:returned-a".into(),
            call_id: "exec-a".into(),
            tokens: Some(7),
        },
        ThreadItemPayload::ToolOutputTokens {
            id: "tool-output-tokens:returned-a".into(),
            call_id: "exec-a".into(),
            tokens: Some(7),
        },
    ] {
        app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(notification(
            &a, item,
        ))));
        drain(&mut app, &mut rx, &mut tui);
    }
    assert_eq!(
        render(&mut app.transcript_view, &app.transcript_cells),
        held
    );
    app.transcript_view.jump_to_latest();
    let current = render(&mut app.transcript_view, &app.transcript_cells);
    assert!(current.contains("Code Mode"));
    assert_eq!(
        app.transcript_cells
            .iter()
            .filter(|cell| cell.as_any().is::<history_cell::CodeModeWaitCell>())
            .count(),
        1
    );
    let terminal = app
        .transcript_cells
        .iter()
        .find_map(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::CodeModeCell>()
                .filter(|block| block.exec_call_id() == "exec-a")
        })
        .unwrap()
        .clone();
    assert!(!terminal.is_active());
    let before_child = terminal.members().to_vec();
    assert_eq!(
        app.transcript_view
            .activity_output_tokens(&terminal.activity_ids()),
        Some(10)
    );
    // An older page's pending child cannot overwrite an already admitted completion.
    app.apply_code_mode_admission(
        &mut tui,
        thread,
        Admission::item(
            CodeModeOwned::new(
                a.clone(),
                Box::new(call("child-a", ActivityKind::Exploration, "inProgress")),
            ),
            "older".into(),
        ),
    );
    let after = app
        .transcript_cells
        .iter()
        .find_map(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::CodeModeCell>()
                .filter(|block| block.exec_call_id() == "exec-a")
        })
        .unwrap();
    assert!(!after.is_active());
    assert_eq!(before_child.len(), after.members().len());
    assert!(
        before_child
            .iter()
            .zip(after.members())
            .all(|(old, new)| Arc::ptr_eq(old, new))
    );

    // Shared fork lineage preserves the origin descriptor, but banks belong to the receiving view.
    // A suffix capture without Finished must remain non-live in each isolated fork.
    let mut banks = Reducers::default();
    let suffix = || {
        Admission::item(
            CodeModeOwned::new(
                a.clone(),
                Box::new(output("suffix", "wait-a", "Late wait capture")),
            ),
            "later-turn".into(),
        )
    };
    let partial_fork = banks
        .admit(
            fork,
            suffix(),
            &app.config.cwd,
            /*animations*/ false,
            &|_| crate::multi_agents::AgentMetadata::default(),
            history_cell::ImagePathOrigin::Local,
        )
        .unwrap();
    let partial_sibling = banks
        .admit(
            sibling,
            suffix(),
            &app.config.cwd,
            /*animations*/ false,
            &|_| crate::multi_agents::AgentMetadata::default(),
            history_cell::ImagePathOrigin::Local,
        )
        .unwrap();
    assert!(
        !partial_fork
            .block
            .as_any()
            .downcast_ref::<history_cell::CodeModeCell>()
            .unwrap()
            .is_active()
    );
    let hydrated_fork = banks.hydrate(fork, &terminal).unwrap();
    assert_eq!(
        banks.hydrate(fork, &terminal).unwrap().raw_lines(),
        hydrated_fork.raw_lines(),
    );
    let unchanged_sibling = banks
        .admit(
            sibling,
            suffix(),
            &app.config.cwd,
            /*animations*/ false,
            &|_| crate::multi_agents::AgentMetadata::default(),
            history_cell::ImagePathOrigin::Local,
        )
        .unwrap();
    assert!(
        !unchanged_sibling
            .block
            .as_any()
            .downcast_ref::<history_cell::CodeModeCell>()
            .unwrap()
            .terminal()
    );
    assert!(
        hydrated_fork
            .as_any()
            .downcast_ref::<history_cell::CodeModeCell>()
            .unwrap()
            .terminal()
    );
    let conflicted = CodeModeOwner {
        cell_id: Some("allocated-a".into()),
        ..a.clone()
    };
    banks
        .admit(
            fork,
            Admission::item(
                CodeModeOwned::new(
                    conflicted.clone(),
                    Box::new(output("allocated", "exec-a", "Allocated")),
                ),
                "turn".into(),
            ),
            &app.config.cwd,
            /*animations*/ false,
            &|_| crate::multi_agents::AgentMetadata::default(),
            history_cell::ImagePathOrigin::Local,
        )
        .unwrap();
    let conflicting = CodeModeOwner {
        cell_id: Some("different-runtime".into()),
        ..conflicted
    };
    assert!(
        banks
            .admit(
                fork,
                Admission::item(
                    CodeModeOwned::new(conflicting, Box::new(output("bad", "exec-a", "Rejected"))),
                    "turn".into()
                ),
                &app.config.cwd,
                /*animations*/ false,
                &|_| crate::multi_agents::AgentMetadata::default(),
                history_cell::ImagePathOrigin::Local,
            )
            .is_err()
    );
    let mut examples = vec![format!("Held reading\n{held}\n\nReturn to live\n{current}")];
    // Terminal bookkeeping is intentionally absent from normal chat paint, but
    // both interactions must survive in detailed transcript history.
    let interactions = before_child
        .iter()
        .filter(|cell| {
            cell.as_any()
                .is::<history_cell::UnifiedExecInteractionCell>()
        })
        .flat_map(|cell| cell.transcript_hyperlink_lines(90))
        .map(|line| line.line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    examples.push(format!("Terminal interactions\n{interactions}"));
    for (heading, cell) in [
        (
            "Exact captures",
            Arc::new(terminal.clone()) as Arc<dyn HistoryCell>,
        ),
        ("Historical suffix", partial_fork.block),
        ("Hydrated fork", hydrated_fork),
        ("Isolated sibling", partial_sibling.block),
    ] {
        let expanded = FoldState::from([cell.activity_ids()[0].clone()]);
        let lines = cell
            .activity_parts(90, /*header_reserve*/ 0, &expanded)
            .unwrap()
            .lines
            .into_iter()
            .map(|line| line.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        examples.push(format!("{heading}\n{lines}"));
    }
    insta::assert_snapshot!("scoped_code_mode_admission", examples.join("\n\n"));

    // A raw execution ID is not ownership: legacy reconciliation must not absorb scoped rows.
    let scoped: Arc<dyn HistoryCell> = Arc::new(terminal.clone());
    let ThreadItemPayload::CodeMode(fact) = code_mode(
        "exec-a",
        CodeModeEvent::ToolCall {
            call_id: "legacy-child".into(),
        },
    ) else {
        unreachable!();
    };
    let legacy_fact: Arc<dyn HistoryCell> = Arc::new(history_cell::CodeModeFactCell(fact, false));
    // The earlier unowned child forces the legacy reconciliation span across the scoped row.
    let unowned = thread_items_to_transcript_cells(
        Some(thread),
        &app.config.cwd,
        [call("legacy-child", ActivityKind::Exploration, "completed")],
        RawReasoningVisibility::Hidden,
        None,
        &|_| crate::multi_agents::AgentMetadata::default(),
        history_cell::ImagePathOrigin::Local,
    )
    .pop()
    .unwrap();
    let mut mixed = vec![Arc::clone(&unowned), Arc::clone(&scoped)];
    let (range, replacements) = history_cell::reconcile_code_mode(&mixed, &legacy_fact).unwrap();
    mixed.splice(range, replacements);
    assert_eq!(mixed.len(), 2);
    assert!(Arc::ptr_eq(&mixed[1], &scoped));
    let legacy = mixed[0]
        .as_any()
        .downcast_ref::<history_cell::CodeModeCell>()
        .unwrap();
    assert!(legacy.owner().is_none());
    assert_eq!(legacy.members().len(), 1);
    assert!(Arc::ptr_eq(&legacy.members()[0], &unowned));

    // Snapshot-native scopes normalize once; a live scope cannot borrow another routed thread.
    let started = ServerNotification::ItemStarted(ItemStartedNotification::new(
        thread.to_string(),
        "turn".into(),
        owned_item(&a, output("snapshot", "exec-a", "Snapshot")),
        0,
    ));
    assert!(
        Admission::notification(&started, Source::Historical)
            .unwrap()
            .is_some()
    );
    let misrouted = ServerNotification::ItemCompleted(ItemCompletedNotification::new(
        fork.to_string(),
        "turn".into(),
        owned_item(&a, output("misrouted", "exec-a", "Rejected")),
        None,
        0,
    ));
    assert!(Admission::notification(&misrouted, Source::Live).is_err());

    let history = vec![
        owned_item(
            &a,
            code_mode(
                "exec-a",
                CodeModeEvent::Started {
                    call_id: "exec-a".into(),
                    script: None,
                },
            ),
        ),
        owned_item(&a, call("child-a", ActivityKind::Exploration, "inProgress")),
        owned_item(&a, call("child-a", ActivityKind::Exploration, "completed")),
        owned_item(
            &a,
            code_mode(
                "exec-a",
                CodeModeEvent::Finished {
                    status: CodeModeStatus::Completed,
                },
            ),
        ),
        owned_item(
            &a,
            code_mode(
                "exec-a",
                CodeModeEvent::WaitStarted {
                    call_id: "wait-a".into(),
                    target: None,
                },
            ),
        ),
        owned_item(
            &a,
            code_mode(
                "exec-a",
                CodeModeEvent::WaitCompleted {
                    call_id: "wait-a".into(),
                    target: None,
                },
            ),
        ),
        owned_item(&a, output("returned-a", "wait-a", "Exact accepted return")),
    ];
    let describe = |cells: &[Arc<dyn HistoryCell>]| {
        cells
            .iter()
            .map(|cell| {
                cell.transcript_hyperlink_lines(/*width*/ 90)
                    .into_iter()
                    .map(|line| line.line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
    };
    let expected = describe(&thread_items_to_transcript_cells(
        Some(thread),
        &app.config.cwd,
        history.clone(),
        RawReasoningVisibility::Hidden,
        /*config*/ None,
        &|_| crate::multi_agents::AgentMetadata::default(),
        history_cell::ImagePathOrigin::Local,
    ));
    assert_prompt_arrival_ordering().await;
    // Work and process inventories identify real calls, not the block's display identity.
    let navigation = thread_items_to_transcript_cells(
        Some(thread),
        &app.config.cwd,
        [
            owned_item(
                &a,
                code_mode(
                    "exec-a",
                    CodeModeEvent::Started {
                        call_id: "exec-a".into(),
                        script: Some("await tools.bash('cat child-a.rs');".into()),
                    },
                ),
            ),
            owned_item(
                &a,
                code_mode(
                    "exec-a",
                    CodeModeEvent::ToolCall {
                        call_id: "fact-only".into(),
                    },
                ),
            ),
            owned_item(
                &a,
                ThreadItemPayload::ToolInput(codex_protocol::tool_input::ToolInput {
                    id: "input:metadata-only".into(),
                    call_id: "metadata-only".into(),
                    name: codex_protocol::ToolName::plain("fs"),
                    payload: codex_protocol::tool_input::ToolInputPayload::Json(
                        r#"{"operation":"read","path":"input"}"#.into(),
                    ),
                }),
            ),
            owned_item(&a, call("child-a", ActivityKind::Exploration, "completed")),
            call("plain-direct", ActivityKind::Exploration, "completed"),
        ],
        RawReasoningVisibility::Hidden,
        None,
        &|_| Default::default(),
        history_cell::ImagePathOrigin::Local,
    );
    assert_eq!(navigation.len(), 2);
    for call_id in [
        "exec-a",
        "fact-only",
        "metadata-only",
        "child-a",
        "plain-direct",
    ] {
        let mut view = crate::transcript_view::TranscriptView::default();
        render(&mut view, &navigation);
        assert!(!view.reveal_call(&navigation, "not-loaded"));
        assert!(view.is_following());
        assert!(view.reveal_call(&navigation, call_id), "{call_id}");
        assert!(!view.is_following());
        assert!(view.is_activity_focused());
        let revealed = render(&mut view, &navigation);
        assert!(
            revealed.contains(if call_id == "plain-direct" {
                "plain-direct.rs"
            } else {
                "child-a.rs"
            }),
            "{call_id}: {revealed}"
        );
        // Repeating Show in history never folds an already visible input away.
        assert!(view.reveal_call(&navigation, call_id));
        assert_eq!(render(&mut view, &navigation), revealed);
    }
    // A sparse scoped suffix can share an outer call ID with a legacy descriptor,
    // but canonical navigation IDs cannot turn one execution block into another's child.
    let sparse: Arc<dyn HistoryCell> = Arc::new(history_cell::CodeModeCell::Scoped(
        crate::code_mode_transcript::ScopedCodeMode::new(a.clone(), false),
    ));
    assert_eq!(sparse.tool_call_ids(), vec!["exec-a"]);
    let mut isolated = vec![Arc::clone(&sparse)];
    let (range, replacements) = history_cell::reconcile_code_mode(&isolated, &legacy_fact).unwrap();
    isolated.splice(range, replacements);
    assert_eq!(isolated.len(), 2);
    assert!(Arc::ptr_eq(&isolated[1], &sparse));
    assert!(
        !isolated[0]
            .as_any()
            .downcast_ref::<history_cell::CodeModeCell>()
            .unwrap()
            .owns(sparse.as_ref())
    );
    for split in 1..history.len() {
        app.reset_transcript_state_after_clear();
        app.transcript_cells = thread_items_to_transcript_cells(
            Some(thread),
            &app.config.cwd,
            history[split..].iter().cloned(),
            RawReasoningVisibility::Hidden,
            /*config*/ None,
            &|_| crate::multi_agents::AgentMetadata::default(),
            history_cell::ImagePathOrigin::Local,
        );
        for cell in app.transcript_cells.clone() {
            if cell.as_any().is::<history_cell::CodeModeCell>() {
                app.insert_code_mode_cell(&mut tui, &cell);
            }
        }
        let older = thread_items_to_transcript_cells(
            Some(thread),
            &app.config.cwd,
            history[..split].iter().cloned(),
            RawReasoningVisibility::Hidden,
            /*config*/ None,
            &|_| crate::multi_agents::AgentMetadata::default(),
            history_cell::ImagePathOrigin::Local,
        );
        let mut page = Vec::new();
        for cell in older {
            app.admit_code_mode_page_cell(&mut tui, cell, &mut page);
        }
        app.transcript_cells.splice(0..0, page);
        assert_eq!(describe(&app.transcript_cells), expected, "split={split}");
    }
}

async fn assert_prompt_arrival_ordering() {
    use codex_app_server_protocol::ArrivalItem;
    use codex_app_server_protocol::ItemCompletedNotification;
    use codex_app_server_protocol::ServerNotification;
    use codex_app_server_protocol::UserInput;
    use codex_protocol::ResponseItemId;
    use codex_protocol::step_admission::ArrivalKind;
    use codex_protocol::step_admission::ArrivalPlacement;
    use codex_protocol::step_admission::ArrivalSide;
    use codex_protocol::step_admission::ArrivalSource;

    let (mut app, mut rx, _op_rx) = crate::app::tests::make_test_app_with_channels().await;
    let thread = ThreadId::new();
    app.chat_widget
        .handle_thread_session(crate::app::tests::test_thread_session(
            thread,
            app.config.cwd.to_path_buf(),
        ));
    while rx.try_recv().is_ok() {}
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    let drain = |app: &mut App,
                 rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
                 tui: &mut crate::tui::Tui| {
        while let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::InsertHistoryCell(cell) => app.insert_history_cell(tui, cell),
                AppEvent::BindUserPrompt {
                    identity,
                    timestamp,
                    ..
                } => app.bind_user_prompt_identity(tui, identity, timestamp),
                _ => {}
            }
        }
    };
    assert!(
        app.chat_widget
            .submit_plain_user_turn_to(thread, "one".into())
            .is_ok()
    );
    drain(&mut app, &mut rx, &mut tui);
    let client_id = app
        .transcript_cells
        .iter()
        .find_map(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::UserHistoryCell>()
                .and_then(|user| user.prompt_identity.client_id.clone())
        })
        .expect("optimistic prompt identity");
    // Submission is not admission: the optimistic prompt has no invented clock.
    assert!(
        app.transcript_cells
            .iter()
            .all(|cell| cell.transcript_timestamp().is_none())
    );
    let receipt_time = 1_700_000_000_000;
    let user = |name: &str, client_id| ThreadItemPayload::UserMessage {
        id: format!("ui-{name}"),
        client_id,
        source_response_item_id: Some(ResponseItemId::from_server(format!("msg-{name}"))),
        content: vec![UserInput::Text {
            text: name.into(),
            text_elements: Vec::new(),
        }],
    };
    let arrival = |id: &str, anchor: Option<&str>, side: ArrivalSide, response: &str| {
        ThreadItemPayload::Arrival(ArrivalItem {
            id: id.into(),
            response_id: response.into(),
            announced_at_ms: 0,
            kind: ArrivalKind::Injection,
            label: id.into(),
            text: None,
            source: ArrivalSource::Captured {
                blocks: vec![id.into()],
            },
            placement: anchor.map(|name| ArrivalPlacement {
                user_response_item_id: ResponseItemId::from_server(format!("msg-{name}")),
                side,
            }),
        })
    };
    // A second prompt is accepted before the first prompt's delayed announcements.
    // Peer batches need no global clock: keep their persisted source order even if
    // a history replacement between batches changed all sampled prompt indices.
    let items = vec![
        user("one", Some(client_id)),
        arrival("legacy-between", None, ArrivalSide::After, "old"),
        user("two", None),
        arrival("before-one-a", Some("one"), ArrivalSide::Before, "parent"),
        arrival("before-one-b", Some("one"), ArrivalSide::Before, "parent"),
        arrival("after-one-a", Some("one"), ArrivalSide::After, "old"),
        arrival("before-two", Some("two"), ArrivalSide::Before, "old"),
        arrival("after-one-b", Some("one"), ArrivalSide::After, "new"),
        arrival("after-two-a", Some("two"), ArrivalSide::After, "new"),
        arrival("after-two-b", Some("two"), ArrivalSide::After, "new"),
        arrival("legacy-end", None, ArrivalSide::Before, "new"),
        arrival("missing-source", Some("absent"), ArrivalSide::Before, "new"),
    ];
    let paint = |app: &mut App| {
        let area = ratatui::layout::Rect::new(0, 0, 90, 4);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        app.transcript_view
            .render(area, &mut buffer, &app.transcript_cells);
        buffer
    };
    let mut held = None;
    for (index, item) in items.iter().enumerate() {
        app.chat_widget.handle_server_notification(
            ServerNotification::ItemCompleted(ItemCompletedNotification::new(
                thread.to_string(),
                "turn".into(),
                item.clone(),
                None,
                receipt_time + i64::try_from(index).expect("short receipt sequence") * 60_000,
            )),
            None,
        );
        drain(&mut app, &mut rx, &mut tui);
        if index == 2 {
            paint(&mut app);
            app.transcript_view.jump_to_entry(&app.transcript_cells, 0);
            held = Some(paint(&mut app));
        }
    }
    assert_eq!(
        Some(paint(&mut app)),
        held,
        "reading snapshot stays stationary during relocation"
    );
    app.transcript_view.jump_to_latest();
    let labels = |cells: &[Arc<dyn HistoryCell>]| {
        cells
            .iter()
            .filter_map(|cell| {
                if let Some(user) = cell
                    .as_any()
                    .downcast_ref::<history_cell::UserHistoryCell>()
                {
                    Some(user.message.clone())
                } else {
                    cell.as_any()
                        .downcast_ref::<history_cell::ArrivalCell>()
                        .map(|arrival| arrival.id().to_string())
                }
            })
            .collect::<Vec<_>>()
    };
    let expected = vec![
        "before-one-a",
        "before-one-b",
        "one",
        "legacy-between",
        "after-one-a",
        "before-two",
        "after-one-b",
        "two",
        "after-two-a",
        "after-two-b",
        "legacy-end",
        "missing-source",
    ];
    assert_eq!(labels(&app.transcript_cells), expected);
    assert_eq!(
        app.transcript_cells
            .iter()
            .filter_map(|cell| {
                cell.as_any()
                    .downcast_ref::<history_cell::UserHistoryCell>()
                    .map(HistoryCell::transcript_timestamp)
            })
            .collect::<Vec<_>>(),
        vec![
            history_cell::HistoryTimestamp::from_millis(receipt_time),
            history_cell::HistoryTimestamp::from_millis(receipt_time + 120_000),
        ],
    );
    assert_eq!(labels(&project(&app, &items)), expected);

    // History envelopes carry the same admission receipts as live notifications.
    // Replaying a steer must not substitute the turn's start or the reload time.
    let mut clocks = crate::prompt_order::UserPromptClocks::default();
    for (index, item) in items.iter().enumerate() {
        clocks.record(&codex_app_server_protocol::ThreadItemEntry {
            turn_id: "turn".into(),
            output_deferred: false,
            started_at_ms: None,
            completed_at_ms: None,
            item: ThreadItem {
                payload: item.clone(),
                started_at_ms: None,
                completed_at_ms: Some(receipt_time + i64::try_from(index).unwrap() * 60_000),
            },
        });
    }
    let timestamps = |cells: &[Arc<dyn HistoryCell>]| {
        cells
            .iter()
            .filter_map(|cell| {
                cell.as_any()
                    .downcast_ref::<history_cell::UserHistoryCell>()
            })
            .map(HistoryCell::transcript_timestamp)
            .collect::<Vec<_>>()
    };
    let expected_clocks = timestamps(&app.transcript_cells);
    let retained = app.transcript_cells.clone();
    app.reconcile_prompt_order(&mut tui);
    assert!(
        retained
            .iter()
            .zip(&app.transcript_cells)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
    for split in 0..=items.len() {
        let mut paged = make_test_app().await;
        paged.transcript_cells = project(&paged, &items[split..]);
        let mut prefix = project(&paged, &items[..split]);
        prefix.append(&mut paged.transcript_cells);
        clocks.stamp(&mut prefix);
        assert_eq!(timestamps(&prefix), expected_clocks);
        paged.transcript_cells = prefix;
        paged.reconcile_prompt_order(&mut tui);
        assert_eq!(
            labels(&paged.transcript_cells),
            expected,
            "page split {split}"
        );
    }
}

fn project(
    app: &App,
    items: &[impl std::borrow::Borrow<ThreadItemPayload>],
) -> Vec<Arc<dyn HistoryCell>> {
    thread_items_to_transcript_cells(
        /*thread_id*/ None,
        &app.config.cwd,
        items.iter().map(|entry| entry.borrow().clone()),
        RawReasoningVisibility::Hidden,
        Some(&app.config),
        &|_| crate::multi_agents::AgentMetadata::default(),
        history_cell::ImagePathOrigin::Local,
    )
}

#[tokio::test]
async fn metadata_only_receipt_keeps_block_disclosure_and_replay_without_a_token_bill() {
    use codex_app_server_protocol::CodeModeEvent;
    use codex_app_server_protocol::CodeModeStatus;

    let (mut app, mut rx, _) = crate::app::tests::make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    tui.set_owned_screen(/*owned*/ true).unwrap();
    let snapshot = |digest: char| serde_json::json!({"uri":"file:///workspace/receipt.rs", "sha256":digest.to_string().repeat(64), "bytes":4});
    let edit: ThreadItemPayload = serde_json::from_value(serde_json::json!({
        "type":"mcpToolCall", "id":"receipt-edit", "server":"semedit", "tool":"edit", "status":"completed",
        "arguments":{}, "durationMs":1,
        "result":{"content":[], "_meta":{"codex/fileChanges":{"version":1,"outcome":"applied","omittedFiles":0,"files":[{
            "kind":"text", "publication":"applied", "operation":"modify", "before":snapshot('0'), "after":snapshot('1'),
            "unifiedDiff":"--- a/receipt.rs\n+++ b/receipt.rs\n@@ -1 +1 @@\n-old\n+new\n"
        }]}}}
    })).unwrap();
    let current = turn(vec![
        code_mode(
            "receipt-exec",
            CodeModeEvent::Started {
                call_id: "receipt-exec".into(),
                script: None,
            },
        ),
        code_mode(
            "receipt-exec",
            CodeModeEvent::ToolCall {
                call_id: "receipt-edit".into(),
            },
        ),
        edit,
        code_mode(
            "receipt-exec",
            CodeModeEvent::ToolCall {
                call_id: "native-edit".into(),
            },
        ),
        ThreadItemPayload::FileChange {
            id: "native-edit".into(),
            changes: vec![
                codex_app_server_protocol::FileUpdateChange {
                    path: "alpha.rs".into(),
                    kind: codex_app_server_protocol::PatchChangeKind::Add,
                    diff: "created body\n".into(),
                },
                codex_app_server_protocol::FileUpdateChange {
                    path: "beta.rs".into(),
                    kind: codex_app_server_protocol::PatchChangeKind::Delete,
                    diff: "deleted body\n".into(),
                },
            ],
            status: codex_app_server_protocol::PatchApplyStatus::Completed,
        },
        code_mode(
            "receipt-exec",
            CodeModeEvent::Finished {
                status: CodeModeStatus::Completed,
            },
        ),
    ]);
    for item in current.item_payloads() {
        app.chat_widget.replay_thread_item(
            item.clone(),
            current.id.clone(),
            crate::chatwidget::ReplayKind::ResumeInitialMessages,
        );
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                app.insert_history_cell(&mut tui, cell);
            }
        }
    }
    let expected = project(&app, &current.items);
    assert_eq!(expected.len(), 1);
    assert_eq!(
        app.transcript_view
            .activity_output_tokens(&expected[0].activity_ids()),
        None
    );
    let describe = |cells: &[Arc<dyn HistoryCell>]| {
        cells
            .iter()
            .map(|cell| cell.expanded_hyperlink_lines(90))
            .collect::<Vec<_>>()
    };
    assert_eq!(describe(&app.transcript_cells), describe(&expected));
    for split in 1..current.items.len() {
        let older = project(&app, &current.items[..split]);
        let boundary = older.len();
        app.transcript_cells = older
            .into_iter()
            .chain(project(&app, &current.items[split..]))
            .collect();
        app.join_older_activity_group(&mut tui, boundary, std::slice::from_ref(&current));
        assert_eq!(
            describe(&app.transcript_cells),
            describe(&expected),
            "split={split}"
        );
    }
    let mut file_snapshots = Vec::new();
    let mut view = crate::transcript_view::TranscriptView::default();
    let render = |view: &mut crate::transcript_view::TranscriptView| {
        let area = ratatui::layout::Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 90, /*height*/ 30,
        );
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        view.render(area, &mut buffer, &expected);
        buffer
            .content()
            .chunks(90)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned()
    };
    let painted = render(&mut view);
    assert_eq!(
        painted
            .matches(crate::render::disclosure::glyph(false).trim())
            .count(),
        3,
        "{painted}"
    );
    assert!(painted.contains("Edited"));
    assert!(painted.contains("receipt.rs"));
    let row = painted
        .lines()
        .position(|line| line.contains("alpha.rs"))
        .unwrap();
    let column = usize::from(crate::transcript_geometry::CONTROL_COLUMN);
    assert!(
        view.handle_mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: u16::try_from(column).unwrap(),
                row: u16::try_from(row).unwrap(),
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            &expected
        )
        .is_some()
    );
    let clicked = render(&mut view);
    assert!(clicked.lines().any(|line| line.contains("alpha.rs")
        && line.contains(crate::render::disclosure::glyph(true).trim())));
    assert!(clicked.contains("created body"));
    assert!(!clicked.contains("deleted body"));
    file_snapshots.push(format!(
        "Painted collapsed\n{painted}\nClicked native file\n{clicked}"
    ));
    let click = |view: &mut crate::transcript_view::TranscriptView,
                 shown: &str,
                 needle: &str,
                 child: Option<&str>| {
        let (row, line) = shown
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(needle))
            .unwrap();
        let mut column = line.find(needle).unwrap();
        if let Some(child) = child {
            column += line[column..].find(child).unwrap();
        }
        assert!(
            view.handle_mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left
                    ),
                    column: u16::try_from(unicode_width::UnicodeWidthStr::width(&line[..column]))
                        .unwrap(),
                    row: u16::try_from(row).unwrap(),
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
                &expected
            )
            .is_some()
        );
    };
    let shown = clicked;
    let receipt_line = shown
        .lines()
        .find(|line| line.contains("receipt.rs"))
        .unwrap();
    let receipt_fold = receipt_line[receipt_line
        .find(crate::render::disclosure::glyph(false).trim())
        .unwrap()..]
        .to_owned();
    click(&mut view, &shown, &receipt_fold, None);
    let shown = render(&mut view);
    assert!(shown.contains("+new"));
    file_snapshots.push(format!("Receipt expanded\n{shown}"));
    click(&mut view, &shown, "[in", None);
    let hidden = render(&mut view);
    assert!(hidden.contains("+new"));
    assert!(hidden.contains("created body"));
    assert!(!hidden.contains("semedit.edit"));
    assert!(hidden.contains("[in]"));
    assert!(!hidden.contains("Exact input is unavailable"));
    file_snapshots.push(format!("Both ports hidden\n{hidden}"));
    click(&mut view, &hidden, "[out]", None);
    let shown = render(&mut view);
    click(&mut view, &shown, "[out", Some("[raw]"));
    let raw = render(&mut view);
    assert!(raw.contains("+new"));
    assert!(raw.contains("created body"));
    assert!(
        raw.lines()
            .find(|line| line.contains("[out"))
            .is_some_and(|line| !line.contains("[fmt]"))
    );
    file_snapshots.push(format!("Output open\n{raw}"));
    insta::assert_snapshot!("metadata_only_code_mode_receipt", file_snapshots.join("\n"));
    // A later model-visible return contributes one bill; receipt bytes and replay never do.
    app.chat_widget.replay_thread_item(
        ThreadItemPayload::FunctionCallOutput {
            id: "receipt-model-output".into(),
            call_id: Some("receipt-exec".into()),
            name: "exec".into(),
            namespace: None,
            output: codex_protocol::models::FunctionCallOutputBody::Text("Visible summary".into()),
        },
        current.id,
        crate::chatwidget::ReplayKind::ResumeInitialMessages,
    );
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            app.insert_history_cell(&mut tui, cell);
        }
    }
    for (id, call, tokens) in [
        ("receipt-model-output", "receipt-exec", 7),
        ("raw-receipt-edit", "receipt-edit", 2048),
        ("receipt-model-output", "receipt-exec", 7),
    ] {
        app.transcript_view.record_tool_output_tokens(
            format!("tool-output-tokens:{id}"),
            call.into(),
            Some(tokens),
        );
    }
    assert_eq!(
        app.transcript_cells
            .iter()
            .map(|cell| app
                .transcript_view
                .activity_output_tokens(&cell.activity_ids()))
            .collect::<Vec<_>>(),
        vec![Some(7)]
    );
}

#[tokio::test]
async fn interleaved_scripts_keep_exact_ownership_after_terminal_wait_and_replay() {
    use codex_app_server_protocol::CodeModeEvent;
    use codex_app_server_protocol::CodeModeStatus;
    use codex_protocol::models::FunctionCallOutputBody;

    let (mut app, mut rx, _) = crate::app::tests::make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    tui.set_owned_screen(/*owned*/ true).unwrap();
    let output = |id: &str, call: &str, name: &str| ThreadItemPayload::FunctionCallOutput {
        id: id.into(),
        call_id: Some(call.into()),
        name: name.into(),
        namespace: None,
        output: FunctionCallOutputBody::Text(format!("visible {id}")),
    };
    let current = turn(vec![
        code_mode(
            "exec-a",
            CodeModeEvent::Started {
                call_id: "exec-a".into(),
                script: Some("text('owned source');".into()),
            },
        ),
        code_mode(
            "exec-a",
            CodeModeEvent::ToolCall {
                call_id: "child-a1".into(),
            },
        ),
        call("child-a1", ActivityKind::Computer, "inProgress"),
        code_mode(
            "exec-b",
            CodeModeEvent::Started {
                call_id: "exec-b".into(),
                script: None,
            },
        ),
        code_mode(
            "exec-b",
            CodeModeEvent::ToolCall {
                call_id: "child-b".into(),
            },
        ),
        call("child-b", ActivityKind::Computer, "inProgress"),
        call("direct", ActivityKind::Computer, "completed"),
        code_mode(
            "exec-a",
            CodeModeEvent::ToolCall {
                call_id: "child-a2".into(),
            },
        ),
        call("child-a2", ActivityKind::Computer, "completed"),
        code_mode(
            "exec-a",
            CodeModeEvent::ToolCall {
                call_id: "view-a".into(),
            },
        ),
        ThreadItemPayload::ImageView {
            id: "view-a".into(),
            path: codex_utils_path_uri::LegacyAppPathString::from_string("diagram.png"),
        },
        code_mode(
            "exec-a",
            CodeModeEvent::ToolCall {
                call_id: "image-a".into(),
            },
        ),
        ThreadItemPayload::ImageGeneration(codex_app_server_protocol::ImageGenerationItem {
            id: "image-a".into(),
            status: "completed".into(),
            revised_prompt: Some("A precise diagram".into()),
            image: codex_app_server_protocol::GeneratedImageRef::Inline {
                result: String::new(),
            },
            input_images: Vec::new(),
            transparent_background: None,
            failure: None,
            saved_path: None,
            imagegen_request_id: None,
            generation_id: None,
        }),
        output("notify-a", "exec-a", "exec"),
        call("child-b", ActivityKind::Computer, "completed"),
        call("child-a1", ActivityKind::Computer, "completed"),
        code_mode(
            "exec-a",
            CodeModeEvent::WaitStarted {
                call_id: "wait-a".into(),
                target: None,
            },
        ),
        code_mode(
            "exec-b",
            CodeModeEvent::Finished {
                status: CodeModeStatus::Interrupted,
            },
        ),
        code_mode(
            "exec-a",
            CodeModeEvent::Finished {
                status: CodeModeStatus::Failed,
            },
        ),
        // Completion can precede consumption. Neither the later reference nor output moves it.
        code_mode(
            "exec-a",
            CodeModeEvent::WaitCompleted {
                call_id: "wait-a".into(),
                target: None,
            },
        ),
        output("wait-output-a", "wait-a", "wait"),
    ]);
    let mut selected_before_completion = None;
    for item in current.item_payloads() {
        if selected_before_completion.is_none()
            && matches!(item, ThreadItemPayload::CodeMode(fact) if matches!(fact.event, CodeModeEvent::Finished { .. }))
        {
            let area = Rect::new(
                /*x*/ 0, /*y*/ 0, /*width*/ 90, /*height*/ 40,
            );
            assert!(
                app.transcript_view
                    .reveal_call(&app.transcript_cells, "exec-a")
            );
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            app.transcript_view
                .render(area, &mut buffer, &app.transcript_cells);
            // Selecting script source is an explicit Raw choice, not the semantic default.
            let (row, column) = buffer
                .content()
                .chunks(usize::from(area.width))
                .enumerate()
                .find_map(|(row, cells)| {
                    cells
                        .iter()
                        .map(ratatui::buffer::Cell::symbol)
                        .collect::<String>()
                        .find("[raw]")
                        .map(|column| (row, column + 1))
                })
                .unwrap();
            app.transcript_view.handle_mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: u16::try_from(column).unwrap(),
                    row: u16::try_from(row).unwrap(),
                    modifiers: KeyModifiers::NONE,
                },
                &app.transcript_cells,
            );
            app.transcript_view
                .render(area, &mut buffer, &app.transcript_cells);
            let source_row = buffer
                .content()
                .chunks(usize::from(area.width))
                .position(|row| {
                    row.iter()
                        .map(ratatui::buffer::Cell::symbol)
                        .collect::<String>()
                        .contains("owned source")
                })
                .unwrap();
            for (kind, column) in [
                (
                    crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    12,
                ),
                (
                    crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                    30,
                ),
                (
                    crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
                    30,
                ),
            ] {
                app.transcript_view.handle_mouse(
                    crossterm::event::MouseEvent {
                        kind,
                        column,
                        row: u16::try_from(source_row).unwrap(),
                        modifiers: KeyModifiers::NONE,
                    },
                    &app.transcript_cells,
                );
            }
            selected_before_completion = app.transcript_view.selected_text(&app.transcript_cells);
            assert!(selected_before_completion.is_some());
        }
        app.chat_widget.handle_server_notification(
            codex_app_server_protocol::ServerNotification::ItemCompleted(
                codex_app_server_protocol::ItemCompletedNotification::new(
                    ThreadId::default().to_string(),
                    current.id.clone(),
                    item.clone(),
                    None,
                    0,
                ),
            ),
            /*replay_kind*/ None,
        );
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                app.insert_history_cell(&mut tui, cell);
            }
        }
    }
    let rendered = |cells: &[Arc<dyn HistoryCell>]| {
        cells
            .iter()
            .map(|cell| {
                (
                    cell.compact_hyperlink_lines(/*width*/ 90),
                    cell.expanded_hyperlink_lines(/*width*/ 90),
                    cell.activity_ids(),
                )
            })
            .collect::<Vec<_>>()
    };
    // Persisted thread history materializes lifecycle updates by item identity.
    let mut materialized = Vec::<ThreadItemPayload>::new();
    for item in current.item_payloads() {
        if let Some(old) = materialized.iter_mut().find(|old| old.id() == item.id()) {
            *old = item.clone();
        } else {
            materialized.push(item.clone());
        }
    }
    let current = turn(materialized);
    let expected = project(&app, &current.items);
    assert_eq!(rendered(&app.transcript_cells), rendered(&expected));
    assert_eq!(
        app.transcript_view.selected_text(&app.transcript_cells),
        selected_before_completion
    );
    assert_eq!(expected.len(), 4);
    let text = |cell: &Arc<dyn HistoryCell>| {
        cell.display_lines(/*width*/ 90)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    // Finished blocks keep their places ahead of the calls and waits that followed them.
    assert!(text(&expected[0]).contains("Failed (Code Mode"));
    assert!(text(&expected[1]).contains("Interrupted (Code Mode"));
    assert!(text(&expected[2]).contains("direct"));
    assert!(text(&expected[3]).contains("Waited for"));
    assert!(!text(&expected[0]).contains("direct"));
    assert!(!text(&expected[0]).contains("child-b"));
    assert!(!text(&expected[0]).contains("visible notify-a"));
    assert!(
        expected[0]
            .expanded_hyperlink_lines(/*width*/ 90)
            .iter()
            .any(|line| line.line.to_string().contains("visible wait-output-a"))
    );
    insta::assert_snapshot!(
        "interleaved_code_mode",
        expected
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let collapsed = text(cell);
                let expanded = cell
                    .expanded_hyperlink_lines(/*width*/ 90)
                    .iter()
                    .map(|line| line.line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("cell {index} collapsed\n{collapsed}\nexpanded\n{expanded}")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    for split in 1..current.items.len() {
        let older = project(&app, &current.items[..split]);
        let boundary = older.len();
        app.transcript_cells = older
            .into_iter()
            .chain(project(&app, &current.items[split..]))
            .collect();
        app.join_older_activity_group(&mut tui, boundary, std::slice::from_ref(&current));
        assert_eq!(
            rendered(&app.transcript_cells),
            rendered(&expected),
            "split={split}"
        );
    }
    for (id, call, count) in [
        ("notify-a", "exec-a", 17),
        ("wait-output-a", "wait-a", 23),
        ("notify-a", "exec-a", 17),
        ("raw-child", "child-a1", 999),
    ] {
        app.transcript_view.record_tool_output_tokens(
            format!("tool-output-tokens:{id}"),
            call.into(),
            Some(count),
        );
    }
    assert_eq!(
        expected
            .iter()
            .map(|cell| app
                .transcript_view
                .activity_output_tokens(&cell.activity_ids()))
            .collect::<Vec<_>>(),
        vec![Some(40), None, None, None]
    );
}

#[tokio::test]
async fn returned_batches_keep_one_disclosure_and_bill_across_live_replay_and_pages() {
    use codex_app_server_protocol::CodeModeEvent;
    use codex_app_server_protocol::CodeModeStatus;
    use codex_protocol::models::FunctionCallOutputBody;
    use codex_protocol::models::FunctionCallOutputContentItem;

    let emitted = "  chosen porcelain:  exact\n{\"content\":[{\"type\":\"text\",\"text\":\"repeat\"}],\"structuredContent\":\"repeat\"}\n{\"content\":[{\"type\":\"text\",\"text\":\"repeat\"}],\"structuredContent\":\"repeat\"}\n";
    let output = |id: &str| ThreadItemPayload::FunctionCallOutput {
        id: id.to_owned(),
        call_id: Some(if id == "b" { "a" } else { id }.to_owned()),
        name: "exec".into(),
        namespace: None,
        output: if id == "a" {
            FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "Script running with cell ID reused-runtime-cell\nWall time 0.1 seconds\nOutput:\n".into(),
                },
                FunctionCallOutputContentItem::InputText { text: emitted.into() },
            ])
        } else {
            // A notify string resembling an execution envelope is still script output.
            FunctionCallOutputBody::Text(format!(
                "Returned {id}\nScript completed\nWall time 0.0 seconds\nOutput:\nOnly model-visible text."
            ))
        },
    };
    let rendered = |cells: &[Arc<dyn HistoryCell>]| {
        cells
            .iter()
            .map(|cell| {
                (
                    cell.compact_hyperlink_lines(/*width*/ 80),
                    cell.expanded_hyperlink_lines(/*width*/ 80),
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
                && let ThreadItemPayload::CommandExecution {
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
            code_mode(
                "a",
                CodeModeEvent::Started {
                    call_id: "a".into(),
                    script: None,
                },
            ),
            code_mode(
                "a",
                CodeModeEvent::ToolCall {
                    call_id: "first".into(),
                },
            ),
            command("first"),
            code_mode(
                "a",
                CodeModeEvent::ToolCall {
                    call_id: "nested".into(),
                },
            ),
            command("nested"),
            output("a"),
            reasoning("between-outputs"),
            output("b"),
            code_mode(
                "a",
                CodeModeEvent::Finished {
                    status: CodeModeStatus::Completed,
                },
            ),
            code_mode(
                "c",
                CodeModeEvent::Started {
                    call_id: "c".into(),
                    script: None,
                },
            ),
            code_mode(
                "c",
                CodeModeEvent::ToolCall {
                    call_id: "second".into(),
                },
            ),
            command("second"),
            output("c"),
            code_mode(
                "c",
                CodeModeEvent::Finished {
                    status: CodeModeStatus::Completed,
                },
            ),
        ]);
        let expected = project(&app, &current.items);
        assert_eq!(expected.len(), 3);
        // A discarded child may contain anything; only returned batches are output authority.
        let mut expanded = FoldState::from_iter(expected[0].activity_ids());
        let folded = expected[0]
            .output_parts(
                240,
                0,
                &expanded,
                history_cell::output_provenance::OutputMode::Raw,
            )
            .unwrap();
        assert!(
            folded
                .controls
                .iter()
                .filter(|control| control.id.starts_with("result:"))
                .all(|control| !control.expanded)
        );
        assert!(
            !folded
                .lines
                .iter()
                .any(|line| line.line.to_string().contains("chosen porcelain"))
        );
        expanded.extend(["result:a".into(), "result:b".into()]);
        let detail = expected[0]
            .output_parts(
                240,
                0,
                &expanded,
                history_cell::output_provenance::OutputMode::Raw,
            )
            .unwrap()
            .lines;
        let text = detail
            .iter()
            .map(|line| line.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("Result first"));
        assert!(!text.contains("Result nested"));
        assert!(text.contains("chosen porcelain:  exact"));
        assert_eq!(text.matches("structuredContent").count(), 2);
        assert!(text.contains("Script completed\n"));
        assert!(
            detail
                .iter()
                .filter_map(|line| line.source.as_ref())
                .any(|source| source.text.as_ref() == "  chosen porcelain:  exact")
        );
        if let ThreadItemPayload::FunctionCallOutput {
            id,
            call_id,
            name,
            namespace,
            output,
        } = output("a")
        {
            let batch = history_cell::ToolOutputCell::from_output(
                &id,
                call_id.as_deref(),
                &name,
                namespace.as_deref(),
                &output,
            )
            .unwrap();
            let rows = batch.transcript_hyperlink_lines(/*width*/ 240);
            let copied = rows
                .iter()
                .map(|line| line.source.as_ref().unwrap().text.as_ref())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(copied, emitted);
        }

        // Nested calls and notify() outputs share a call; a different call starts a new unit.
        // Reconnect and notification backpressure can deliver ownership after visible rows.
        // The live path must reach the same projection as a complete replay without a reload.
        let (ownership, operations): (Vec<_>, Vec<_>) = current.item_payloads().partition(|item| {
            matches!(item, ThreadItemPayload::CodeMode(fact) if matches!(fact.event,
                CodeModeEvent::Started { .. } | CodeModeEvent::ToolCall { .. }))
        });
        for item in operations.into_iter().chain(ownership) {
            if let ThreadItemPayload::Reasoning { id, summary, .. } = item {
                app.chat_widget.handle_server_notification(
                    codex_app_server_protocol::ServerNotification::ItemStarted(
                        codex_app_server_protocol::ItemStartedNotification::new(
                            ThreadId::default().to_string(),
                            current.id.clone(),
                            item.clone(),
                            0,
                        ),
                    ),
                    /*replay_kind*/ None,
                );
                for (index, delta) in summary.iter().enumerate() {
                    app.chat_widget.handle_server_notification(
                        codex_app_server_protocol::ServerNotification::ReasoningSummaryTextDelta(
                            codex_app_server_protocol::ReasoningSummaryTextDeltaNotification {
                                thread_id: ThreadId::default().to_string(),
                                turn_id: current.id.clone(),
                                item_id: id.clone(),
                                delta: delta.clone(),
                                summary_index: index as i64,
                            },
                        ),
                        /*replay_kind*/ None,
                    );
                }
            }
            app.chat_widget.handle_server_notification(
                codex_app_server_protocol::ServerNotification::ItemCompleted(
                    codex_app_server_protocol::ItemCompletedNotification::new(
                        ThreadId::default().to_string(),
                        current.id.clone(),
                        item.clone(),
                        None,
                        0,
                    ),
                ),
                /*replay_kind*/ None,
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
            app.join_older_activity_group(&mut tui, boundary, std::slice::from_ref(&current));
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
            vec![Some(1280), None, Some(20)]
        );
        for expanded in [false, true] {
            app.transcript_view.jump_to_latest();
            let area = Rect::new(
                /*x*/ 0, /*y*/ 0, /*width*/ 100, /*height*/ 32,
            );
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            app.transcript_view.render(area, &mut buffer, &expected);
            if expanded {
                let (row, column) = buffer
                    .content()
                    .chunks(usize::from(area.width))
                    .enumerate()
                    .find_map(|(row, cells)| {
                        let line = cells
                            .iter()
                            .map(ratatui::buffer::Cell::symbol)
                            .collect::<String>();
                        line.find("[out]").map(|column| {
                            (row, unicode_width::UnicodeWidthStr::width(&line[..column]))
                        })
                    })
                    .unwrap();
                app.transcript_view.handle_mouse(
                    crossterm::event::MouseEvent {
                        kind: crossterm::event::MouseEventKind::Down(
                            crossterm::event::MouseButton::Left,
                        ),
                        column: u16::try_from(column).unwrap(),
                        row: u16::try_from(row).unwrap(),
                        modifiers: KeyModifiers::NONE,
                    },
                    &expected,
                );
                app.transcript_view.jump_to_latest();
                buffer = ratatui::buffer::Buffer::empty(area);
                app.transcript_view.render(area, &mut buffer, &expected);
            }
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
            assert_eq!(lines.contains("chosen porcelain"), expanded);
            snapshots.push(format!(
                "{kind:?} ordinary={ordinary} expanded={expanded}\n{}",
                lines.trim()
            ));
        }
    }
    insta::assert_snapshot!("coalesced_returned_batches", snapshots.join("\n\n"));

    // Exact call ownership must survive both transport direction and every page split,
    // while the capture has its own output-item identity and initially closed disclosure.
    use codex_app_server_protocol::CollabAgentTool;
    use codex_app_server_protocol::CollabAgentToolCallStatus;
    use codex_protocol::agent_wait::AgentWaitCompletion;
    use codex_protocol::agent_wait::AgentWaitOutcome;
    let wait = |status| ThreadItemPayload::CollabAgentToolCall {
        id: "wait-p".into(),
        tool: CollabAgentTool::Wait,
        status,
        sender_thread_id: ThreadId::default().to_string(),
        receiver_thread_ids: Vec::new(),
        prompt: None,
        model: None,
        reasoning_effort: None,
        agents_states: Default::default(),
        wait_completion: Some(AgentWaitCompletion {
            outcome: AgentWaitOutcome::TimedOut,
            elapsed_ms: 50,
        }),
    };
    let captured = r#"{"message":"Wait timed out.","timed_out":true}"#;
    let result = ThreadItemPayload::FunctionCallOutput {
        id: "wait-output".into(),
        call_id: Some("wait-p".into()),
        name: "wait_agent".into(),
        namespace: Some("wrangler_agents".into()),
        output: FunctionCallOutputBody::Text(captured.into()),
    };
    // A nested wait retains its human outcome even when JS discards its return.
    // The outer capture is not evidence that the nested result was model-visible.
    let (app, _, _) = crate::app::tests::make_test_app_with_channels().await;
    let owner = codex_app_server_protocol::CodeModeOwner {
        thread_id: ThreadId::default(),
        exec_call_id: "outer-exec".into(),
        cell_id: None,
        started_at_ms: 0,
    };
    let scoped = |item| {
        ThreadItemPayload::CodeModeOwned(codex_app_server_protocol::CodeModeOwned::new(
            owner.clone(),
            Box::new(item),
        ))
    };
    let scoped_cells = project(
        &app,
        &[
            scoped(wait(CollabAgentToolCallStatus::Completed)),
            scoped(ThreadItemPayload::FunctionCallOutput {
                id: "outer-output".into(),
                call_id: Some("outer-exec".into()),
                name: "exec".into(),
                namespace: None,
                output: FunctionCallOutputBody::Text("JS kept only this".into()),
            }),
            scoped(wait(CollabAgentToolCallStatus::InProgress)),
        ],
    );
    assert_eq!(scoped_cells.len(), 1);
    let block = scoped_cells[0]
        .as_any()
        .downcast_ref::<history_cell::CodeModeCell>()
        .unwrap();
    let nested = block
        .members()
        .iter()
        .find(|cell| crate::multi_agents::AgentToolHistoryCell::wait_for(cell.as_ref()).is_some())
        .unwrap();
    assert!(
        nested.display_lines(120)[0]
            .to_string()
            .contains("no updates before timeout")
    );
    assert!(nested.raw_lines().is_empty());
    assert!(
        !block
            .raw_lines()
            .iter()
            .any(|line| line.to_string().contains(captured))
    );
    let external = project(&app, std::slice::from_ref(&result)).pop().unwrap();
    assert!(history_cell::reconcile_agent_wait(&scoped_cells, &external).is_none());
    let no_owner = history_cell::ToolOutputCell::from_output(
        "old-output",
        None,
        "wait_agent",
        Some("wrangler_agents"),
        &FunctionCallOutputBody::Text(captured.into()),
    )
    .unwrap();
    assert_eq!(no_owner.outer_call_id(), None);
    for namespace in [None, Some("legacy")] {
        let legacy = history_cell::ToolOutputCell::from_output(
            "legacy-result",
            Some("unknown-call"),
            "wait_agent",
            namespace,
            &FunctionCallOutputBody::Text(captured.into()),
        )
        .unwrap();
        assert!(
            legacy.display_lines(120)[0]
                .to_string()
                .contains("wait_agent")
        );
        assert!(!legacy.is_agent_wait());
        let legacy = Arc::new(legacy) as Arc<dyn HistoryCell>;
        assert!(history_cell::reconcile_agent_wait(&scoped_cells, &legacy).is_none());
    }
    assert!(
        no_owner.display_lines(120)[0]
            .to_string()
            .contains("wrangler_agents.wait_agent")
    );
    for result_first in [false, true] {
        let (mut app, mut rx, _) = crate::app::tests::make_test_app_with_channels().await;
        let mut tui = crate::tui::test_support::make_test_tui().unwrap();
        tui.set_owned_screen(true).unwrap();
        let start = wait(CollabAgentToolCallStatus::InProgress);
        let end = wait(CollabAgentToolCallStatus::Completed);
        let unrelated = call("unrelated-q", ActivityKind::Exploration, "completed");
        let items = if result_first {
            vec![
                result.clone(),
                unrelated.clone(),
                start.clone(),
                end,
                result.clone(),
                start,
            ]
        } else {
            vec![
                start.clone(),
                unrelated.clone(),
                end,
                result.clone(),
                result.clone(),
                start,
            ]
        };
        let current = turn(items);
        let expected = project(&app, &current.items);
        assert_eq!(expected.len(), 2);
        let wait_group = expected
            .iter()
            .find(|cell| cell.activity_ids().contains(&"agent-wait:wait-p".into()))
            .unwrap();
        assert!(
            wait_group.compact_hyperlink_lines(120)[0]
                .line
                .to_string()
                .contains("no updates before timeout")
        );
        assert_eq!(
            wait_group
                .activity_ids()
                .iter()
                .filter(|id| id.starts_with("output:"))
                .count(),
            1
        );
        assert_eq!(
            wait_group
                .raw_lines()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            captured
        );
        let mut expanded = wait_group.activity_ids().into_iter().collect::<FoldState>();
        let parts = wait_group
            .output_parts(
                120,
                0,
                &expanded,
                history_cell::output_provenance::OutputMode::Raw,
            )
            .unwrap();
        assert_eq!(parts.controls.len(), 1);
        assert_eq!(parts.controls[0].id, "result:wait-output");
        assert!(!parts.controls[0].expanded);
        assert!(
            !parts
                .lines
                .iter()
                .any(|line| line.line.to_string().contains(captured))
        );
        expanded.insert("result:wait-output".into());
        let parts = wait_group
            .output_parts(
                120,
                0,
                &expanded,
                history_cell::output_provenance::OutputMode::Raw,
            )
            .unwrap();
        let copied = parts
            .lines
            .iter()
            .filter_map(|line| line.source.as_ref())
            .filter(|source| {
                source.output_provenance
                    == history_cell::output_provenance::OutputProvenance::Captured
            })
            .map(|source| source.text.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(copied, captured);
        for item in current.item_payloads() {
            app.chat_widget.handle_server_notification(
                codex_app_server_protocol::ServerNotification::ItemCompleted(
                    codex_app_server_protocol::ItemCompletedNotification::new(
                        ThreadId::default().to_string(),
                        current.id.clone(),
                        item.clone(),
                        None,
                        0,
                    ),
                ),
                None,
            );
            while let Ok(event) = rx.try_recv() {
                if let AppEvent::InsertHistoryCell(cell) = event {
                    app.insert_history_cell(&mut tui, cell);
                }
            }
        }
        app.chat_widget
            .add_to_history(history_cell::PlainHistoryCell::new(vec!["Answer".into()]));
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                app.insert_history_cell(&mut tui, cell);
            }
        }
        assert_eq!(
            rendered(&app.transcript_cells[..app.transcript_cells.len() - 1]),
            rendered(&expected)
        );
        for split in 1..current.items.len() {
            let older = project(&app, &current.items[..split]);
            let boundary = older.len();
            app.transcript_cells = older
                .into_iter()
                .chain(project(&app, &current.items[split..]))
                .collect();
            app.join_older_activity_group(&mut tui, boundary, std::slice::from_ref(&current));
            assert_eq!(
                rendered(&app.transcript_cells),
                rendered(&expected),
                "wait result_first={result_first} split={split}"
            );
        }
        assert_eq!(
            rendered(&project(&app, &[unrelated])),
            rendered(
                &expected
                    .iter()
                    .filter(|cell| !cell.activity_ids().contains(&"agent-wait:wait-p".into()))
                    .cloned()
                    .collect::<Vec<_>>()
            )
        );
    }
}

#[tokio::test]
async fn legacy_patch_files_keep_independent_outputs_across_live_replay_and_pages() {
    use crate::chatwidget::ReplayKind;
    use codex_app_server_protocol::FileUpdateChange;
    use codex_app_server_protocol::PatchApplyStatus;
    use codex_app_server_protocol::PatchChangeKind;
    use codex_protocol::models::FunctionCallOutputBody;

    // Legacy output without explicit script ownership must not inherit a neighboring patch.
    let output = |id: &str| ThreadItemPayload::FunctionCallOutput {
        id: id.into(),
        call_id: Some(id.into()),
        name: "exec".into(),
        namespace: None,
        output: FunctionCallOutputBody::Text(format!("Returned {id}")),
    };
    let current = turn(vec![
        ThreadItemPayload::FileChange {
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
    // Terminal status must revise requested files even if its authoritative map is empty.
    // A same-path invocation and an already-attached exact capture make identity errors visible.
    for status in [
        PatchApplyStatus::Completed,
        PatchApplyStatus::Failed,
        PatchApplyStatus::Declined,
    ] {
        for empty_terminal in [false, true] {
            let (mut app, mut rx, _) = crate::app::tests::make_test_app_with_channels().await;
            let mut tui = crate::tui::test_support::make_test_tui().unwrap();
            tui.set_owned_screen(/*owned*/ true).unwrap();
            let mut pending = current.items[0].payload.clone();
            if let ThreadItemPayload::FileChange { status, .. } = &mut pending {
                *status = PatchApplyStatus::InProgress;
            }
            let mut unrelated = pending.clone();
            if let ThreadItemPayload::FileChange { id, .. } = &mut unrelated {
                *id = "other-patch".into();
            }
            let capture = ThreadItemPayload::FunctionCallOutput {
                id: "patch-output".into(),
                call_id: Some("patch".into()),
                name: "apply_patch".into(),
                namespace: None,
                output: FunctionCallOutputBody::Text(
                    std::iter::once("exact patch capture".to_string())
                        .chain((1..=12).map(|line| format!("diagnostic line {line}")))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
            };
            for item in [pending.clone(), capture, unrelated] {
                app.chat_widget.replay_thread_item(
                    item,
                    current.id.clone(),
                    ReplayKind::ResumeInitialMessages,
                );
                while let Ok(event) = rx.try_recv() {
                    if let AppEvent::InsertHistoryCell(cell) = event {
                        app.insert_history_cell(&mut tui, cell);
                    }
                }
            }
            assert_eq!(app.transcript_cells.len(), 2);
            let ids = app
                .transcript_cells
                .iter()
                .map(|cell| cell.activity_ids())
                .collect::<Vec<_>>();
            let unrelated_cells = app.transcript_cells[1..].to_vec();
            let mut terminal = pending.clone();
            if let ThreadItemPayload::FileChange {
                status: outcome,
                changes,
                ..
            } = &mut terminal
            {
                *outcome = status.clone();
                if empty_terminal {
                    changes.clear();
                }
            }
            for item in [terminal.clone(), pending.clone()] {
                app.chat_widget.replay_thread_item(
                    item,
                    current.id.clone(),
                    ReplayKind::ResumeInitialMessages,
                );
                while let Ok(event) = rx.try_recv() {
                    if let AppEvent::InsertHistoryCell(cell) = event {
                        app.insert_history_cell(&mut tui, cell);
                    }
                }
            }
            assert_eq!(app.transcript_cells.len(), 2);
            assert_eq!(
                app.transcript_cells
                    .iter()
                    .map(|cell| cell.activity_ids())
                    .collect::<Vec<_>>(),
                ids,
            );
            assert!(
                app.transcript_cells[..1]
                    .iter()
                    .all(|cell| !history_cell::has_running_tool_calls(cell.as_ref()))
            );
            assert!(
                app.transcript_cells[1..]
                    .iter()
                    .all(|cell| history_cell::has_running_tool_calls(cell.as_ref()))
            );
            assert!(
                app.transcript_cells[1..]
                    .iter()
                    .zip(&unrelated_cells)
                    .all(|(cell, old)| Arc::ptr_eq(cell, old))
            );
            let summary = app.transcript_cells[..1]
                .iter()
                .flat_map(|cell| cell.compact_hyperlink_lines(/*width*/ 80))
                .map(|line| line.line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            let heading = match status {
                PatchApplyStatus::Completed if empty_terminal => "No changes",
                PatchApplyStatus::Completed => "Edited",
                PatchApplyStatus::Failed => "Failed to apply patch",
                PatchApplyStatus::Declined => "Patch application declined",
                PatchApplyStatus::InProgress => unreachable!(),
            };
            assert!(summary.contains(heading), "{summary}");
            if status != PatchApplyStatus::Completed || empty_terminal {
                assert!(!summary.contains("Edited"), "{summary}");
                assert_eq!(summary.matches("— requested").count(), 2);
            }
            let raw = app
                .transcript_cells
                .iter()
                .flat_map(|cell| cell.raw_lines())
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(raw.matches("exact patch capture").count(), 1);
            assert!(raw.contains("diagnostic line 12"));

            // A snapshot can already know the outcome when a buffered start arrives.
            app.transcript_cells = project(&app, &[terminal]);
            let terminal_count = app.transcript_cells.len();
            app.chat_widget.replay_thread_item(
                pending,
                current.id.clone(),
                ReplayKind::ResumeInitialMessages,
            );
            while let Ok(event) = rx.try_recv() {
                if let AppEvent::InsertHistoryCell(cell) = event {
                    app.insert_history_cell(&mut tui, cell);
                }
            }
            assert_eq!(app.transcript_cells.len(), terminal_count);
            assert!(
                app.transcript_cells
                    .iter()
                    .all(|cell| !history_cell::has_running_tool_calls(cell.as_ref()))
            );
        }
    }
    for (file_count, with_command) in [(2, false), (2, true), (1, true)] {
        let mut current = current.clone();
        if let ThreadItemPayload::FileChange { changes, .. } = &mut current.items[0].payload {
            changes.truncate(file_count);
        }
        if with_command {
            let mut command = call("command", ActivityKind::Exploration, "completed");
            if let ThreadItemPayload::CommandExecution {
                command,
                command_actions,
                ..
            } = &mut command
            {
                *command = "cargo check".into();
                command_actions.clear();
            }
            current
                .items
                .insert(/*index*/ 1, ThreadItem::untimed(command));
        }
        let (mut app, mut rx, _) = crate::app::tests::make_test_app_with_channels().await;
        let mut tui = crate::tui::test_support::make_test_tui().unwrap();
        tui.set_owned_screen(/*owned*/ true).unwrap();
        let expected = project(&app, &current.items);
        assert_eq!(expected.len(), 1 + usize::from(with_command) + 3);
        for item in current.item_payloads() {
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
            app.join_older_activity_group(&mut tui, boundary, std::slice::from_ref(&current));
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
                    if index == 1 + usize::from(with_command) {
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
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    for kind in [ActivityKind::Computer, ActivityKind::Exploration] {
        let mut app = make_test_app().await;
        match kind {
            ActivityKind::Computer => {
                app.chat_widget
                    .handle_mcp_tool_call_completed_now(ThreadItem::untimed(call(
                        "newer",
                        kind,
                        "completed",
                    )));
                app.chat_widget.handle_mcp_tool_call_started_now(call(
                    "pending",
                    kind,
                    "inProgress",
                ));
            }
            ActivityKind::Exploration => {
                app.chat_widget
                    .handle_command_execution_completed_now(ThreadItem::untimed(call(
                        "newer",
                        kind,
                        "completed",
                    )));
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
        app.join_older_activity_group(
            &mut tui,
            /*boundary*/ 1,
            std::slice::from_ref(&current),
        );
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
                    .handle_mcp_tool_call_completed_now(ThreadItem::untimed(call(
                        "pending",
                        kind,
                        "completed",
                    )))
            }
            ActivityKind::Exploration => {
                app.chat_widget
                    .handle_command_execution_completed_now(ThreadItem::untimed(call(
                        "pending",
                        kind,
                        "completed",
                    )))
            }
        }
        current.items[3] = ThreadItem::untimed(call("pending", kind, "completed"));
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
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    for kind in [ActivityKind::Computer, ActivityKind::Exploration] {
        let mut app = make_test_app().await;
        let mut same_turn = reasoning("same-turn-detail");
        let mut next_turn = reasoning("next-turn-detail");
        for item in [&mut same_turn, &mut next_turn] {
            if let ThreadItemPayload::Reasoning { summary, .. } = item {
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
            app.join_older_activity_group(&mut tui, /*boundary*/ 1, &turns);
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
        app.join_older_activity_group(&mut tui, /*boundary*/ 1, &turns);
        assert_eq!(app.transcript_cells.len(), 2);
        assert!(Arc::ptr_eq(&app.transcript_cells[1], &unidentified));
    }
}

#[tokio::test]
async fn every_page_split_folds_reasoning_before_answer_and_completion_boundaries() {
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    for kind in [ActivityKind::Computer, ActivityKind::Exploration] {
        for answer in [None, Some("The answer")] {
            let mut app = make_test_app().await;
            let mut items = vec![call("call", kind, "completed"), reasoning("detail")];
            if let Some(text) = answer {
                items.push(ThreadItemPayload::AgentMessage {
                    moderation: None,
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
                app.join_older_activity_group(&mut tui, boundary, std::slice::from_ref(&current));
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
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    let mut app = make_test_app().await;
    let mut failed = call("failed", ActivityKind::Exploration, "failed");
    if let ThreadItemPayload::CommandExecution { exit_code, .. } = &mut failed {
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
        app.join_older_activity_group(&mut tui, boundary, std::slice::from_ref(&current));
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
