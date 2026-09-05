use super::*;
use codex_protocol::items::AgentMessageDelivery;
use codex_protocol::items::AsyncUserInputQuestion;
use pretty_assertions::assert_eq;

fn question(id: &str, title: &str, options: Option<Vec<String>>) -> AgentMessageItem {
    AgentMessageItem {
        id: id.to_string(),
        content: vec![AgentMessageContent::Text {
            text: title.to_string(),
        }],
        phase: Some(MessagePhase::FinalAnswer),
        memory_citation: None,
        delivery: Some(AgentMessageDelivery::Async),
        questions: Some(vec![AsyncUserInputQuestion {
            title: title.to_string(),
            options,
        }]),
    }
}

// Async input shares the sync picker but must never inherit its interrupt or
// tool-response behavior, including when another question arrives mid-answer.
#[tokio::test]
async fn async_questions_keep_draft_and_submit_only_the_open_picker() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.on_task_started();
    chat.bottom_pane
        .set_composer_text("unfinished steering".to_string(), Vec::new(), Vec::new());
    let draft = chat.bottom_pane.composer_draft_snapshot();
    let first = question(
        "q1",
        "Which environment should I deploy to?",
        Some(vec!["Staging".into(), "Production".into()]),
    );
    chat.on_agent_message_item_completed(first.clone(), "turn", /*from_replay*/ false);
    chat.on_agent_message_item_completed(first, "turn", /*from_replay*/ false);
    assert!(!chat.bottom_pane.has_active_view());
    assert_chatwidget_snapshot!(
        "async_questions_pending",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    assert_chatwidget_snapshot!(
        "async_questions_pending_narrow",
        render_bottom_popup(&chat, /*width*/ 36)
    );
    chat.dispatch_command(SlashCommand::Questions);
    assert_chatwidget_snapshot!(
        "async_questions_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::from(KeyCode::Esc));
    assert!(!chat.bottom_pane.has_active_view());
    assert_eq!(chat.bottom_pane.composer_draft_snapshot(), draft);
    assert!(op_rx.try_recv().is_err());
    chat.dispatch_command(SlashCommand::Questions);
    chat.receive_async_questions(&question("q2", "Which region?", None));
    let mut buffered = chat.capture_async_questions();
    buffered.receive(
        "q3",
        &[AsyncUserInputQuestion {
            title: "Which account?".into(),
            options: None,
        }],
    );
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let response = loop {
        if let AppEvent::AsyncQuestionsAnswered {
            thread_id: actual,
            response,
        } = rx.try_recv().expect("answer event")
        {
            assert_eq!(actual, thread_id);
            break response;
        }
    };
    chat.answer_async_questions(response);
    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected ordinary user input")
    };
    assert_eq!(
        items,
        vec![UserInput::Text {
            text: "In reply to \"Which environment should I deploy to?\":\nStaging".into(),
            text_elements: Vec::new(),
        }]
    );
    assert_eq!(chat.bottom_pane.composer_draft_snapshot(), draft);
    let rendered = render_bottom_popup(&chat, /*width*/ 80);
    assert!(rendered.contains("1 pending question"));
    assert!(rendered.contains("Which region?"));
    let input = chat.capture_thread_input_state();
    chat.restore_thread_input_state(
        input,
        ThreadInputStateRestoreMode {
            preserve_in_flight_turn: true,
        },
    );
    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("Which region?"));
    chat.merge_async_questions(&buffered);
    // An unseen background question survives; the answered question stays dead.
    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("2 pending questions"));
}

#[tokio::test]
async fn async_questions_freeform_and_dismiss_do_not_interrupt() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.receive_async_questions(&question("q1", "Which region?", None));
    chat.dispatch_command(SlashCommand::Questions);
    chat.handle_paste("us-east-1".into());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let response = loop {
        if let AppEvent::AsyncQuestionsAnswered { response, .. } =
            rx.try_recv().expect("answer event")
        {
            break response;
        }
    };
    chat.answer_async_questions(response);
    assert!(matches!(next_submit_op(&mut op_rx), Op::UserTurn { .. }));
    chat.receive_async_questions(&question("q2", "Delete the deployment?", None));
    chat.dispatch_command(SlashCommand::Questions);
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    let response = loop {
        if let AppEvent::AsyncQuestionsAnswered { response, .. } =
            rx.try_recv().expect("dismiss event")
        {
            break response;
        }
    };
    chat.answer_async_questions(response);
    assert!(op_rx.try_recv().is_err());
    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("pending question"));
}
