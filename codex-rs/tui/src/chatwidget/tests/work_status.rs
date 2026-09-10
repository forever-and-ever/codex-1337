use super::*;
use codex_protocol::protocol::WorkStatus;
use pretty_assertions::assert_eq;
use ratatui::backend::TestBackend;

#[tokio::test]
async fn work_status_below_composer_clears_without_transcript_entries() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.bottom_pane
        .set_composer_text("Draft stays editable".into(), Vec::new(), Vec::new());
    let status = WorkStatus {
        turn_id: "turn-1".into(),
        activity: "Check lifecycle and narrow-terminal wrapping".into(),
    };
    let turn = Turn {
        id: "turn-1".into(),
        items_view: codex_app_server_protocol::TurnItemsView::Full,
        items: Vec::new(),
        status: TurnStatus::InProgress,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    };
    chat.replay_thread_turns(vec![turn.clone()], ReplayKind::ThreadSnapshot);
    let notification = ServerNotification::ThreadWorkStatusUpdated(
        codex_app_server_protocol::ThreadWorkStatusUpdatedNotification {
            thread_id: chat.thread_id.unwrap().to_string(),
            status: status.clone(),
        },
    );
    chat.handle_server_notification(notification.clone(), /*replay_kind*/ None);
    assert_eq!(chat.current_work_status, Some(status.clone()));
    assert!(drain_insert_history(&mut rx).is_empty());
    let mut panels = Vec::new();
    for goal in [
        None,
        Some("Ship useful status updates without cluttering the transcript"),
    ] {
        chat.current_goal_status = goal.map(|objective| {
            GoalStatusState::new(
                codex_app_server_protocol::ThreadGoal {
                    thread_id: chat.thread_id.unwrap().to_string(),
                    objective: objective.into(),
                    status: codex_app_server_protocol::ThreadGoalStatus::Active,
                    token_budget: None,
                    tokens_used: 0,
                    time_used_seconds: 0,
                    created_at: 0,
                    updated_at: 0,
                },
                Instant::now(),
            )
        });
        for width in [80, 30] {
            let height = chat.desired_height(width);
            let mut terminal = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| chat.render(frame.area(), frame.buffer_mut()))
                .unwrap();
            panels.push(normalized_backend_snapshot(terminal.backend()));
            let (_, cursor_y) = chat
                .cursor_pos(Rect::new(0, 0, width, height))
                .expect("composer cursor");
            let status_height = super::super::work_status::WorkStatusView {
                goal,
                status: Some(&status),
            }
            .desired_height(width);
            assert!(cursor_y < height - status_height);
        }
    }
    insta::assert_snapshot!("work_status_below_composer", panels.join("\n\n"));
    for final_status in [
        TurnStatus::Completed,
        TurnStatus::Interrupted,
        TurnStatus::Failed,
    ] {
        chat.replay_thread_turns(vec![turn.clone()], ReplayKind::ThreadSnapshot);
        chat.handle_server_notification(notification.clone(), /*replay_kind*/ None);
        chat.handle_turn_completed_notification(
            TurnCompletedNotification {
                thread_id: chat.thread_id.unwrap().to_string(),
                turn: Turn {
                    status: final_status,
                    items: Vec::new(),
                    ..turn.clone()
                },
            },
            Some(ReplayKind::ThreadSnapshot),
        );
        assert_eq!(chat.current_work_status, None);
        assert!(chat.current_goal_status.is_some());
        // Late completion metadata must not resurrect the previous turn's activity.
        chat.handle_server_notification(notification.clone(), /*replay_kind*/ None);
        assert_eq!(chat.current_work_status, None);
    }
    chat.current_goal_status = None;
    chat.bottom_pane
        .set_composer_text(String::new(), Vec::new(), Vec::new());
    let ids: Vec<_> = (0..9).map(|_| ThreadId::new()).collect();
    {
        let mut roster = chat.subagent_roster.lock().unwrap();
        roster.reconcile(
            ids.iter()
                .enumerate()
                .map(|(n, id)| (*id, format!("worker-{n}")))
                .collect(),
            None,
        );
        for row in &mut roster.rows {
            row.status = Some(codex_app_server_protocol::ThreadStatus::Idle);
            row.tokens = Some(12_345);
        }
        roster.rows[8].activity = Some("Checking the parser and its integration tests".into());
        roster.rows[8].status = Some(codex_app_server_protocol::ThreadStatus::Active {
            active_flags: Vec::new(),
        });
        for _ in 0..9 {
            roster.down();
        }
    }
    let mut panels = Vec::new();
    for watching in [false, true] {
        if watching {
            chat.set_parent_owned_thread();
            let mut roster = chat.subagent_roster.lock().unwrap();
            roster.watched = Some(ids[8]);
            roster.focused = None;
        }
        for (width, max_height) in [(80, 16), (30, 16), (30, 8)] {
            let height = chat.desired_height(width).min(max_height);
            let mut terminal = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| chat.render(frame.area(), frame.buffer_mut()))
                .unwrap();
            assert_eq!(
                terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .any(|cell| cell.modifier.contains(ratatui::style::Modifier::REVERSED)),
                watching
            );
            panels.push(normalized_backend_snapshot(terminal.backend()));
            assert_eq!(chat.cursor_pos(Rect::new(0, 0, width, height)), None);
        }
    }
    insta::assert_snapshot!("subagent_roster_below_activity", panels.join("\n\n"));
}
