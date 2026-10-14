use super::*;

#[test]
fn thread_event_store_restores_active_turn_from_snapshot_turns() {
    use codex_app_server_protocol::CodeModeEvent;
    use codex_app_server_protocol::CodeModeItem;
    use codex_app_server_protocol::CodeModeOwned;
    use codex_app_server_protocol::CodeModeOwner;
    use codex_app_server_protocol::CodeModeStatus;

    fn event(thread: ThreadId, turn: &str, item: ThreadItem, started: bool) -> ServerNotification {
        if started {
            ServerNotification::ItemStarted(codex_app_server_protocol::ItemStartedNotification {
                thread_id: thread.to_string(),
                turn_id: turn.to_owned(),
                item,
                started_at_ms: 1,
            })
        } else {
            ServerNotification::ItemCompleted(
                codex_app_server_protocol::ItemCompletedNotification {
                    thread_id: thread.to_string(),
                    turn_id: turn.to_owned(),
                    item,
                    completed_at_ms: 2,
                },
            )
        }
    }
    fn dynamic(id: &str, status: &str) -> ThreadItem {
        serde_json::from_value(serde_json::json!({
            "type": "dynamicToolCall", "id": id, "namespace": "functions",
            "tool": "python", "arguments": {"source": "1 + 1"}, "status": status,
            "contentItems": null, "success": null, "durationMs": null,
        }))
        .unwrap()
    }
    fn command(status: &str) -> ThreadItem {
        serde_json::from_value(serde_json::json!({
            "type": "commandExecution", "id": "command", "command": "sleep 20",
            "cwd": "/tmp/project", "processId": "session", "status": status,
            "commandActions": [], "aggregatedOutput": null, "exitCode": null,
            "durationMs": null,
        }))
        .unwrap()
    }

    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    let owner = CodeModeOwner {
        thread_id,
        exec_call_id: "exec".to_owned(),
        cell_id: Some("cell".to_owned()),
        started_at_ms: 1,
    };
    let owned = |item| ThreadItem::CodeModeOwned(CodeModeOwned::new(owner.clone(), Box::new(item)));
    let fact = |id: &str, event| {
        owned(ThreadItem::CodeMode(CodeModeItem {
            id: id.to_owned(),
            cell_id: owner.cell_id.clone(),
            exec_call_id: owner.exec_call_id.clone(),
            created_at: 1,
            event,
        }))
    };
    let started = fact(
        "start",
        CodeModeEvent::Started {
            call_id: "exec".to_owned(),
            script: Some("await tool()".to_owned()),
        },
    );
    // A yielded cell and its command belong to their original owner, not the current turn.
    let turns = vec![
        test_turn(
            "turn-1",
            TurnStatus::Completed,
            vec![started.clone(), owned(command("inProgress"))],
        ),
        test_turn(
            "turn-2",
            TurnStatus::InProgress,
            vec![dynamic("direct", "inProgress")],
        ),
    ];
    let mut store = ThreadEventStore::new_with_session(8, session.clone(), turns.clone());
    assert_eq!(store.active_turn_id(), Some("turn-2"));
    assert_eq!(store.turns, turns);
    assert_eq!(store.operations.rows(thread_id).len(), 2);
    assert!(store.operations.rows(ThreadId::new()).is_empty());

    let mut refreshed_store = ThreadEventStore::new(8);
    refreshed_store.set_session(session.clone(), turns.clone());
    assert_eq!(refreshed_store.active_turn_id(), Some("turn-2"));
    assert_eq!(refreshed_store.operations.rows(thread_id).len(), 2);

    let target = CodeModeOwner {
        thread_id: ThreadId::new(),
        exec_call_id: "target-exec".to_owned(),
        cell_id: Some("target-cell".to_owned()),
        started_at_ms: 5,
    };
    let wait = fact(
        "wait-start",
        CodeModeEvent::WaitStarted {
            call_id: "wait".to_owned(),
            target: Some(target.clone()),
        },
    );
    store.push_notification(event(thread_id, "turn-2", wait.clone(), false));
    let rows = store.operations.rows(thread_id);
    let execution = rows.iter().find(|row| row.children.len() == 2).unwrap();
    let wait_row = execution
        .children
        .iter()
        .find(|row| row.summary.to_string().contains("target-cell"))
        .unwrap();
    assert!(
        wait_row.details[0]
            .to_string()
            .contains(&target.thread_id.to_string())
    );

    store.push_notification(event(
        thread_id,
        "turn-2",
        fact(
            "wait-end",
            CodeModeEvent::WaitCompleted {
                call_id: "wait".to_owned(),
                target: Some(target),
            },
        ),
        false,
    ));
    store.push_notification(event(thread_id, "turn-2", wait, false));
    store.push_notification(event(
        thread_id,
        "turn-2",
        owned(command("completed")),
        false,
    ));
    store.push_notification(event(
        thread_id,
        "turn-1",
        owned(command("inProgress")),
        true,
    ));
    store.push_notification(event(
        thread_id,
        "turn-2",
        fact(
            "nested-call",
            CodeModeEvent::ToolCall {
                call_id: "nested".to_owned(),
            },
        ),
        false,
    ));
    store.push_notification(event(
        thread_id,
        "turn-2",
        owned(ThreadItem::ToolOutputTokens {
            id: "nested-tokens".to_owned(),
            call_id: "nested".to_owned(),
            tokens: Some(1),
        }),
        false,
    ));
    store.push_notification(event(
        thread_id,
        "turn-1",
        owned(dynamic("nested", "inProgress")),
        true,
    ));
    assert!(
        store
            .operations
            .rows(thread_id)
            .iter()
            .all(|row| row.children.is_empty())
    );

    // The replay buffer may evict starts; current operations must not disappear with them.
    store.push_notification(event(
        thread_id,
        "turn-2",
        ThreadItem::ContextCompaction {
            id: "compact".to_owned(),
        },
        true,
    ));
    for index in 0..12 {
        store.push_notification(event(
            thread_id,
            "turn-2",
            ThreadItem::AgentMessage {
                id: format!("message-{index}"),
                text: "capture".to_owned(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            },
            false,
        ));
    }
    assert_eq!(store.operations.rows(thread_id).len(), 3);
    store.push_notification(event(
        ThreadId::new(),
        "turn-2",
        dynamic("foreign", "inProgress"),
        true,
    ));
    assert_eq!(store.operations.rows(thread_id).len(), 3);

    store.push_notification(turn_completed_notification(
        thread_id,
        "turn-2",
        TurnStatus::Completed,
    ));
    assert_eq!(store.operations.rows(thread_id).len(), 1);
    store.push_notification(turn_started_notification(thread_id, "turn-3"));
    store.push_notification(turn_completed_notification(
        thread_id,
        "turn-2",
        TurnStatus::Interrupted,
    ));
    store.push_notification(event(
        thread_id,
        "turn-2",
        dynamic("direct", "inProgress"),
        true,
    ));
    assert_eq!(store.active_turn_id(), Some("turn-3"));
    assert_eq!(store.operations.rows(thread_id).len(), 1);

    let finished = fact(
        "end",
        CodeModeEvent::Finished {
            status: CodeModeStatus::Interrupted,
        },
    );
    store.push_notification(event(thread_id, "turn-3", finished.clone(), false));
    store.push_notification(event(thread_id, "turn-1", started.clone(), false));
    assert!(store.operations.rows(thread_id).is_empty());
    refreshed_store.set_session(
        session.clone(),
        vec![
            test_turn("turn-1", TurnStatus::Completed, vec![started, finished]),
            test_turn("turn-3", TurnStatus::InProgress, Vec::new()),
        ],
    );
    refreshed_store.set_session(session, turns);
    assert!(
        refreshed_store
            .operations
            .rows(thread_id)
            .iter()
            .all(|row| row.children.is_empty())
    );
    assert_eq!(refreshed_store.operations.rows(thread_id).len(), 1); // only the current direct call

    store.push_notification(ServerNotification::ThreadClosed(
        codex_app_server_protocol::ThreadClosedNotification {
            thread_id: thread_id.to_string(),
        },
    ));
    store.push_notification(event(
        thread_id,
        "turn-3",
        dynamic("late", "inProgress"),
        true,
    ));
    assert!(store.operations.rows(thread_id).is_empty());
}
