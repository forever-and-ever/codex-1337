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
}
