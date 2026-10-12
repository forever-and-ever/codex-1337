use super::*;
use crate::app::QueueEvent;
use crate::app::session_lifecycle::ThreadAttachPresentation;
use crate::app_command::PendingSteerCancelAction;
use crate::chatwidget::UserMessage;
use codex_app_server_protocol::QueuedSubmission;
use codex_app_server_protocol::TurnToolOutput;
use codex_protocol::models::FunctionCallOutputBody;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn queued_edit_acknowledgement_preserves_the_original_threads_draft() -> Result<()> {
    let (mut app, _home) = make_history_test_app().await?;
    let (server, _, proxy) = start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let original = ThreadId::new();
    let item = QueuedSubmission {
        id: "incoming".into(),
        client_user_message_id: "incoming".into(),
        input: Vec::new(),
        tool_output: Some(Box::new(TurnToolOutput {
            name: "message".into(),
            namespace: None,
            output: FunctionCallOutputBody::Text("follow-up".into()),
        })),
    };
    // A response may arrive after switching threads, or after dispatch won the race.
    // Uncertain removal recovers a draft, never an automatic resubmission.
    for (result, expected) in [
        (Ok(true), "follow-up\noriginal draft"),
        (Ok(false), "original draft"),
        (Err("connection lost".into()), "follow-up\noriginal draft"),
    ] {
        app.chat_widget.apply_external_edit(expected.into());
        let expected = app.chat_widget.capture_thread_input_state();
        app.chat_widget.apply_external_edit("original draft".into());
        let input = app.chat_widget.capture_thread_input_state();
        app.ensure_thread_channel(original)
            .store
            .lock()
            .await
            .input_state = input;
        app.chat_widget.apply_external_edit("current draft".into());
        app.queued_messages.mutations.insert(
            original,
            super::super::queued_messages::QueueMutation {
                item: item.clone(),
                action: PendingSteerCancelAction::Edit,
            },
        );
        app.handle_queue_event(
            &server,
            QueueEvent::Removed {
                thread_id: original,
                result,
            },
        )
        .await;
        assert!(app.queued_messages.mutations.is_empty());
        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "current draft"
        );
        let store = app.thread_event_channels[&original].store.lock().await;
        assert_eq!(store.input_state, expected);
    }
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}
#[tokio::test]
async fn stalled_submission_does_not_block_queued_edit_or_typing() -> Result<()> {
    let (mut app, _home) = make_history_test_app().await?;
    // This case tests submission routing, not the paste-burst first-character delay.
    app.local_settings.tui.disable_paste_burst = Some(true);
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    app.app_event_tx = AppEventSender::new(tx);
    let (mut server, requests, proxy) = start_recording_app_server_with_history(
        &app.config,
        HistoryCapabilities::TurnSubmissionStalls,
        None,
        None,
        crate::app_server_session::ThreadParamsMode::Embedded,
        LoaderOverrides::without_managed_config_for_tests(),
    )
    .await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let started = server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        started,
        ThreadAttachPresentation::SessionLineage,
        None,
    )
    .await?;
    while events.try_recv().is_ok() {}
    let rich_inputs = |text: &str| {
        vec![
            codex_app_server_protocol::UserInput::Text {
                text: text.into(),
                text_elements: vec![
                    codex_protocol::user_input::TextElement::new(
                        codex_protocol::user_input::ByteRange {
                            start: 0,
                            end: text.len(),
                        },
                        None,
                    )
                    .into(),
                ],
            },
            codex_app_server_protocol::UserInput::LocalImage {
                path: "queue-image.png".into(),
                detail: None,
            },
            codex_app_server_protocol::UserInput::Image {
                image: codex_app_server_protocol::ImageReference::Inline {
                    url: "data:image/png;base64,cXVldWU=".into(),
                },
                detail: None,
            },
        ]
    };
    let recovered_message = |app: &mut App| {
        let input = serde_json::to_value(app.chat_widget.capture_thread_input_state()).unwrap();
        let mut composer = input["composer"].clone();
        composer.as_object_mut().unwrap().remove("pending_pastes");
        serde_json::from_value::<UserMessage>(composer).unwrap()
    };
    let rich_prompt = ChatWidget::submission_draft_from_inputs(
        "fixture".into(),
        &rich_inputs("stalled ordinary prompt"),
    )
    .message;
    app.chat_widget.snapshot_local_images = false;
    let first = app
        .chat_widget
        .submit_user_message_as_plain_user_turn(rich_prompt.clone())
        .expect("submit ordinary prompt");
    // This failed against the synchronous routing: no subsequent App event could be serviced.
    tokio::time::timeout(
        Duration::from_secs(5),
        app.submit_thread_op(&mut server, thread_id, first),
    )
    .await
    .expect("server-controlled submission blocked the App loop")?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "turn/start").is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker submitted the first request");
    // A popup send uses the same thread lane, and therefore cannot overtake this request.
    app.enqueue_subagent_turn(
        &server,
        thread_id,
        None,
        vec![codex_app_server_protocol::UserInput::Text {
            text: "waiting follow-up".into(),
            text_elements: Vec::new(),
        }],
    );
    assert_eq!(recorded_params(&requests, "turn/start").len(), 1);
    let preview = render_bottom_popup(&app.chat_widget, 80);
    assert!(preview.contains("Sending") && preview.contains("stalled ordinary prompt"));
    assert!(preview.contains("Waiting to send") && preview.contains("waiting follow-up"));

    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT),
    )
    .await;
    let edit = loop {
        if let event @ AppEvent::Submission(_) = events.recv().await.expect("queued edit event") {
            break event;
        }
    };
    Box::pin(app.handle_event(&mut tui, &mut server, edit)).await?;
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "waiting follow-up"
    );
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;
    let typed = app.chat_widget.composer_text_with_pending();
    assert_eq!(typed.replace('x', ""), "waiting follow-up");
    assert!(
        typed.contains('x'),
        "typing serviced while sending: {typed:?}"
    );
    // App events remain dispatchable as well as keys (including farm-style worker completions).
    Box::pin(app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::InsertHistoryCell(Box::new(history_cell::PlainHistoryCell::new(vec![
            "completion while sending".into(),
        ]))),
    ))
    .await?;
    assert!(app.transcript_cells.iter().any(|cell| {
        cell.display_lines(80)
            .iter()
            .any(|line| line.to_string().contains("completion while sending"))
    }));
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;
    assert!(
        app.chat_widget.composer_text_with_pending().is_empty(),
        "Ctrl+C serviced while the server is stalled"
    );
    // Restore a live draft to pin recovery order.
    app.chat_widget.apply_external_edit("live draft".into());
    app.enqueue_subagent_turn(
        &server,
        thread_id,
        None,
        vec![codex_app_server_protocol::UserInput::Text {
            text: "unsent successor".into(),
            text_elements: Vec::new(),
        }],
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    let completion = app
        .thread_delivery
        .completion_rx
        .recv()
        .await
        .expect("uncertain receipt");
    app.handle_local_thread_completion(&mut server, completion)
        .await?;
    tokio::time::resume();
    assert!(app.server_operations.is_empty());
    assert_eq!(app.chat_widget.composer_text_with_pending(), "live draft");
    let recovered = render_bottom_popup(&app.chat_widget, 100);
    assert!(
        recovered.find("stalled ordinary prompt").unwrap()
            < recovered.find("unsent successor").unwrap()
    );
    assert!(app.chat_widget.has_queued_follow_up_messages());
    assert!(!app.chat_widget.maybe_send_next_queued_input());
    for expected in ["unsent successor", "stalled ordinary prompt"] {
        app.handle_tui_event(
            &mut tui,
            &mut server,
            TuiEvent::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT)),
        )
        .await?;
        assert_eq!(app.chat_widget.composer_text_with_pending(), expected);
        if expected == "stalled ordinary prompt" {
            assert_eq!(recovered_message(&mut app), rich_prompt);
        }
    }
    assert_eq!(recorded_params(&requests, "turn/start").len(), 1);
    assert!(recorded_params(&requests, "turn/steer").is_empty());
    assert!(recorded_params(&requests, "turn/steer/cancel").is_empty());
    assert!(recorded_params(&requests, "thread/queue/delete").is_empty());
    // A reconnect retires the original thread's lane even after its widget was replaced.
    app.chat_widget.apply_external_edit("original draft".into());
    for text in ["disconnected send", "queued after disconnect"] {
        app.enqueue_subagent_turn(&server, thread_id, None, rich_inputs(text));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "turn/start").len() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker submitted the reconnecting request");
    let other = server.start_thread(&app.config).await?;
    let other_id = other.session.thread_id;
    app.ensure_thread_channel(other_id)
        .store
        .lock()
        .await
        .set_session(other.session, other.turns);
    app.select_agent_thread(&mut tui, &mut server, other_id)
        .await?;
    assert_eq!(app.chat_widget.thread_id(), Some(other_id));
    app.chat_widget
        .apply_external_edit("other thread draft".into());
    Box::pin(app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::Submission(crate::app::turn_submissions::SubmissionEvent::Retire),
    ))
    .await?;
    assert!(app.server_operations.is_empty());
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "other thread draft"
    );
    let restored = app.thread_event_channels[&thread_id]
        .store
        .lock()
        .await
        .input_state
        .clone();
    app.chat_widget.restore_thread_input_state(
        restored,
        crate::chatwidget::ThreadInputStateRestoreMode {
            preserve_in_flight_turn: false,
        },
    );
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "original draft"
    );
    let recovered = render_bottom_popup(&app.chat_widget, 100);
    assert!(
        recovered.find("disconnected send").unwrap()
            < recovered.find("queued after disconnect").unwrap()
    );
    assert!(!app.chat_widget.maybe_send_next_queued_input());
    for expected in ["queued after disconnect", "disconnected send"] {
        app.handle_tui_event(
            &mut tui,
            &mut server,
            TuiEvent::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT)),
        )
        .await?;
        assert_eq!(app.chat_widget.composer_text_with_pending(), expected);
        let rich =
            ChatWidget::submission_draft_from_inputs("fixture".into(), &rich_inputs(expected))
                .message;
        assert_eq!(recovered_message(&mut app), rich);
    }
    assert_eq!(recorded_params(&requests, "turn/start").len(), 2);
    // The same owner orders non-turn mutations and connection barriers while input stays live.
    let skills_before = recorded_params(&requests, "skills/list").len();
    let reloads_before = recorded_params(&requests, "config/batchWrite").len();
    let renames_before = recorded_params(&requests, "thread/name/set").len();
    app.submit_thread_op(
        &mut server,
        other_id,
        AppCommand::ListSkills {
            cwds: vec![app.config.cwd.to_path_buf()],
            force_reload: false,
        },
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "skills/list").len() == skills_before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("skills worker reached withheld response");
    // Urgent control bypasses the withheld thread lane and its connection config barrier.
    app.submit_thread_op(&mut server, other_id, AppCommand::ReloadUserConfig)
        .await?;
    let urgent_store = Arc::clone(&app.thread_event_channels[&other_id].store);
    urgent_store
        .lock()
        .await
        .set_active_turn_id("urgent-test".into());
    let interrupts_before = recorded_params(&requests, "turn/interrupt").len();
    app.submit_thread_op(&mut server, other_id, AppCommand::interrupt())
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "turn/interrupt").len() < interrupts_before + 1 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    let control = app
        .server_operations
        .control_rx
        .recv()
        .await
        .expect("unknown interrupt receipt");
    app.finish_submission(
        &mut server,
        control.thread_id,
        control.id,
        control.epoch,
        control.outcome,
    )
    .await?;
    tokio::time::resume();
    assert!(
        urgent_store
            .lock()
            .await
            .pending_interrupt_turn_id
            .is_none()
    );
    assert_eq!(
        recorded_params(&requests, "turn/interrupt").len(),
        interrupts_before + 1
    );
    // An unknown receipt never resends itself, but does not disable a new deliberate control.
    app.submit_thread_op(&mut server, other_id, AppCommand::interrupt())
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "turn/interrupt").len() < interrupts_before + 2 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    app.submit_thread_op(
        &mut server,
        thread_id,
        AppCommand::SetThreadName {
            name: "queued rename".into(),
        },
    )
    .await?;
    assert_eq!(
        recorded_params(&requests, "config/batchWrite").len(),
        reloads_before
    );
    assert_eq!(
        recorded_params(&requests, "thread/name/set").len(),
        renames_before
    );
    let preview = render_bottom_popup(&app.chat_widget, 100);
    assert!(preview.contains("Loading skills") && preview.contains("Reloading settings"));
    app.chat_widget.apply_external_edit("RPC draft".into());
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;
    assert!(app.chat_widget.composer_text_with_pending().contains('x'));
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;
    assert!(app.chat_widget.composer_text_with_pending().is_empty());
    let rename_id = match app.server_operations.preview(Some(thread_id)) {
        crate::app::turn_submissions::SubmissionPreview::Sending { waiting, .. } => waiting
            .into_iter()
            .find_map(|entry| match entry {
                crate::app::turn_submissions::SubmissionPreviewEntry::Operation {
                    id,
                    text: "Renaming conversation",
                } => Some(id),
                _ => None,
            })
            .expect("queued mutation remains visible and cancellable"),
        _ => panic!("queued mutation preview"),
    };
    app.handle_submission_event(
        &server,
        crate::app::turn_submissions::SubmissionEvent::Edit {
            thread_id,
            id: rename_id,
            action: crate::app_command::PendingSteerCancelAction::Remove,
        },
    )
    .await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    let completion = app
        .thread_delivery
        .completion_rx
        .recv()
        .await
        .expect("skills timeout");
    app.handle_local_thread_completion(&mut server, completion)
        .await?;
    tokio::time::resume();
    app.settle_turn_submissions(&mut server).await?;
    assert_eq!(
        recorded_params(&requests, "skills/list").len(),
        skills_before + 1
    );
    assert_eq!(
        recorded_params(&requests, "config/batchWrite").len(),
        reloads_before + 1
    );
    assert_eq!(
        recorded_params(&requests, "thread/name/set").len(),
        renames_before
    );
    assert!(app.server_operations.is_empty());
    // Retirement releases definitively unsent alarm/compaction leases, never sends them.
    app.submit_thread_op(
        &mut server,
        other_id,
        AppCommand::ListSkills {
            cwds: vec![app.config.cwd.to_path_buf()],
            force_reload: false,
        },
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "skills/list").len() < skills_before + 2 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    app.disarm_alarm(&mut server, other_id, "queued-alarm".into())
        .await;
    assert!(
        !app.chat_widget
            .begin_alarm_disarm(other_id, "different-alarm")
    );
    app.chat_widget.apply_external_edit("/compact".into());
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .await;
    let compact = loop {
        if let event @ AppEvent::CodexOp(AppCommand::Compact) =
            events.recv().await.expect("compact event")
        {
            break event;
        }
    };
    Box::pin(app.handle_event(&mut tui, &mut server, compact)).await?;
    app.handle_submission_event(
        &server,
        crate::app::turn_submissions::SubmissionEvent::Retire,
    )
    .await;
    assert!(
        app.chat_widget
            .begin_alarm_disarm(other_id, "after-retirement")
    );
    app.chat_widget
        .cancel_unsent_alarm_disarm("after-retirement");
    let input = serde_json::to_value(app.chat_widget.capture_thread_input_state())?;
    assert_eq!(input["input_queue"]["turn"], "Idle");
    assert!(recorded_params(&requests, "thread/alarm/disarm").is_empty());
    assert!(recorded_params(&requests, "thread/compact/start").is_empty());
    assert!(app.server_operations.is_empty());
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}
