use super::*;
use pretty_assertions::assert_eq;

#[test]
fn thread_event_store_restores_active_turn_from_snapshot_turns() {
    use codex_app_server_protocol::CodeModeEvent;
    use codex_app_server_protocol::CodeModeItem;
    use codex_app_server_protocol::CodeModeOwned;
    use codex_app_server_protocol::CodeModeOwner;
    use codex_app_server_protocol::CodeModeStatus;

    fn event(
        thread: ThreadId,
        turn: &str,
        item: ThreadItemPayload,
        started: bool,
    ) -> ServerNotification {
        if started {
            ServerNotification::ItemStarted(ItemStartedNotification::new(
                thread.to_string(),
                turn.to_owned(),
                item,
                1,
            ))
        } else {
            ServerNotification::ItemCompleted(ItemCompletedNotification::new(
                thread.to_string(),
                turn.to_owned(),
                item,
                None,
                2,
            ))
        }
    }
    fn dynamic(id: &str, status: &str) -> ThreadItemPayload {
        serde_json::from_value(serde_json::json!({
            "type": "dynamicToolCall", "id": id, "namespace": "functions",
            "tool": "python", "arguments": {"source": "1 + 1"}, "status": status,
            "contentItems": null, "success": null, "durationMs": null,
        }))
        .unwrap()
    }
    fn command(status: &str) -> ThreadItemPayload {
        serde_json::from_value(serde_json::json!({
            "type": "commandExecution", "id": "command", "command": "/usr/bin/zsh -lc 'sleep 20'",
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
    let owned =
        |item| ThreadItemPayload::CodeModeOwned(CodeModeOwned::new(owner.clone(), Box::new(item)));
    let fact = |id: &str, event| {
        owned(ThreadItemPayload::CodeMode(CodeModeItem {
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
    // The old daemon emitted an unowned review start but owned canonical events.
    // Identity, not the envelope, must own the row and its terminal obligation.
    for terminal in [
        Some(owned(command("completed"))),
        Some(command("completed")),
        None,
    ] {
        let mut reviewed = ThreadEventStore::new_with_session(
            8,
            session.clone(),
            vec![test_turn("review-turn", TurnStatus::InProgress, Vec::new())],
        );
        reviewed.push_notification(event(thread_id, "review-turn", command("inProgress"), true));
        reviewed.push_notification(event(
            thread_id,
            "review-turn",
            owned(command("inProgress")),
            true,
        ));
        let rows = reviewed.operations.rows(thread_id);
        assert_eq!(
            rows.len(),
            1,
            "owned start re-parents the review placeholder"
        );
        assert_eq!(rows[0].children.len(), 1);
        if let Some(terminal) = terminal {
            reviewed.push_notification(event(thread_id, "review-turn", terminal, false));
            assert!(
                reviewed.operations.rows(thread_id).is_empty(),
                "either envelope terminates the item"
            );
        }
        reviewed.push_notification(event(
            thread_id,
            "review-turn",
            fact(
                "review-finished",
                CodeModeEvent::Finished {
                    status: CodeModeStatus::Completed,
                },
            ),
            false,
        ));
        assert!(
            reviewed.operations.rows(thread_id).is_empty(),
            "the execution reaps a missing item terminal"
        );
        reviewed.push_notification(event(thread_id, "review-turn", command("inProgress"), true));
        assert!(
            reviewed.operations.rows(thread_id).is_empty(),
            "late duplicate starts cannot resurrect an item"
        );
    }

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
    assert_eq!(
        store
            .operations
            .rows(thread_id)
            .iter()
            .flat_map(|row| &row.children)
            .next()
            .unwrap()
            .summary
            .to_string(),
        "Command · sleep 20"
    );
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
    store.push_notification(event(
        thread_id,
        "turn-2",
        fact(
            "wait-call",
            CodeModeEvent::ToolCall {
                call_id: "wait".to_owned(),
            },
        ),
        false,
    ));
    store.push_notification(event(thread_id, "turn-2", wait.clone(), false));
    let rows = store.operations.rows(thread_id);
    let execution = rows.iter().find(|row| row.children.len() == 2).unwrap();
    let wait_row = execution
        .children
        .iter()
        .find(|row| {
            row.summary.to_string()
                == format!(
                    "Waiting for ({})",
                    history_cell::code_mode_owner_label(&target)
                )
        })
        .unwrap();
    assert!(
        wait_row.details[0]
            .to_string()
            .contains(&target.thread_id.to_string())
    );

    assert_eq!(
        execution.summary.to_string(),
        format!("Running ({})", history_cell::code_mode_owner_label(&owner))
    );
    store.push_notification(event(
        thread_id,
        "turn-2",
        owned(command("completed")),
        false,
    ));
    let waiting = store.operations.rows(thread_id);
    assert!(
        waiting
            .iter()
            .all(|row| !row.summary.to_string().starts_with("Running"))
    );
    assert!(waiting.iter().any(|row| row.summary == wait_row.summary));
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
        owned(ThreadItemPayload::ToolOutputTokens {
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
        ThreadItemPayload::ContextCompaction {
            id: "compact".to_owned(),
        },
        true,
    ));
    for index in 0..12 {
        store.push_notification(event(
            thread_id,
            "turn-2",
            ThreadItemPayload::AgentMessage {
                id: format!("message-{index}"),
                text: "capture".to_owned(),
                phase: None,
                memory_citation: None,
                moderation: None,
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

    // Approval starts are placeholders, not independent process lifetimes. A
    // terminal command or its enclosing cell must retire them without a turn change.
    for status in ["completed", "failed", "declined"] {
        let mut approval = command("inProgress");
        if let ThreadItemPayload::CommandExecution { id, process_id, .. } = &mut approval {
            *id = format!("approval-{status}");
            *process_id = None;
        }
        store.push_notification(event(thread_id, "turn-3", owned(approval.clone()), true));
        assert_eq!(store.operations.rows(thread_id)[0].children.len(), 1);
        if let ThreadItemPayload::CommandExecution {
            status: outcome, ..
        } = &mut approval
        {
            *outcome = serde_json::from_value(serde_json::json!(status)).unwrap();
        }
        store.push_notification(event(thread_id, "turn-3", owned(approval), false));
        assert!(store.operations.rows(thread_id)[0].children.is_empty());
    }
    let mut approval = command("inProgress");
    if let ThreadItemPayload::CommandExecution { id, process_id, .. } = &mut approval {
        *id = "approval-cell-finish".to_owned();
        *process_id = None;
    }
    store.push_notification(event(thread_id, "turn-3", owned(approval), true));
    let other_owner = CodeModeOwner {
        exec_call_id: "other-exec".to_owned(),
        cell_id: Some("other-cell".to_owned()),
        ..owner.clone()
    };
    let other_owned = |item| {
        ThreadItemPayload::CodeModeOwned(CodeModeOwned::new(other_owner.clone(), Box::new(item)))
    };
    let mut other_command = command("inProgress");
    if let ThreadItemPayload::CommandExecution { id, .. } = &mut other_command {
        *id = "other-command".to_owned();
    }
    store.push_notification(event(
        thread_id,
        "turn-3",
        other_owned(other_command.clone()),
        true,
    ));
    store.push_notification(event(thread_id, "turn-3", command("inProgress"), true));

    let finished = fact(
        "end",
        CodeModeEvent::Finished {
            status: CodeModeStatus::Interrupted,
        },
    );
    store.push_notification(event(thread_id, "turn-3", finished.clone(), false));
    store.push_notification(event(thread_id, "turn-1", started.clone(), false));
    assert_eq!(store.operations.rows(thread_id).len(), 1);
    if let ThreadItemPayload::CommandExecution { status, .. } = &mut other_command {
        *status = codex_app_server_protocol::CommandExecutionStatus::Completed;
    }
    store.push_notification(event(
        thread_id,
        "turn-3",
        other_owned(other_command),
        false,
    ));
    store.push_notification(event(thread_id, "turn-3", command("completed"), false));
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
