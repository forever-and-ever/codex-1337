use super::super::turn_submissions::ui_requests::UiRequestKind;
use super::super::turn_submissions::ui_requests::UiRequestScope;
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
    let (mut app, _home) = Box::pin(make_history_test_app()).await?;
    let (server, _, proxy) = Box::pin(start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    ))
    .await?;
    let original = ThreadId::new();
    let item = QueuedSubmission {
        responsesapi_client_metadata: None,
        id: "incoming".into(),
        client_user_message_id: "incoming".into(),
        input: Vec::new(),
        tool_output: Some(Box::new(TurnToolOutput {
            retain: false,
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
            queued_messages::QueueMutation {
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
    let (mut app, _home) = Box::pin(make_history_test_app()).await?;
    // This case tests submission routing, not the paste-burst first-character delay.
    app.local_settings.tui.disable_paste_burst = Some(true);
    let (tx, mut events) = unbounded_channel();
    app.app_event_tx = AppEventSender::new(tx);
    let (mut server, requests, proxy) = Box::pin(start_recording_app_server_with_history(
        &app.config,
        HistoryCapabilities::TurnSubmissionStalls,
        None,
        None,
        crate::app_server_session::ThreadParamsMode::Embedded,
        LoaderOverrides::without_managed_config_for_tests(),
    ))
    .await?;
    let mut tui = tui::test_support::make_test_tui()?;
    let started = Box::pin(server.start_thread(&app.config)).await?;
    let thread_id = started.session.thread_id;
    Box::pin(app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        started,
        ThreadAttachPresentation::SessionLineage,
        None,
    ))
    .await?;
    while events.try_recv().is_ok() {}
    // A stalled typed request must not hold the UI task. Its synchronous
    // continuation stays ahead of an already-queued connection barrier.

    let applied = Arc::new(Mutex::new(Vec::new()));
    let (release, stalled) = oneshot::channel();
    let first_applied = Arc::clone(&applied);
    app.request_server(
        &server,
        UiRequestScope::Connection,
        UiRequestKind::ReadOnly,
        "stalled UI request",
        move |_| async move { Ok(stalled.await.expect("release stalled UI request")) },
        move |app, server, value| {
            first_applied.lock().unwrap().push(value.unwrap());
            app.request_server(
                server,
                UiRequestScope::Connection,
                UiRequestKind::ReadOnly,
                "dependent UI request",
                |_| async { Ok(2_u32) },
                move |_, _, value| {
                    first_applied.lock().unwrap().push(value.unwrap());
                    Ok(())
                },
            );
            Ok(())
        },
    );
    let successor_applied = Arc::clone(&applied);
    let prepared_after = Arc::clone(&applied);
    app.request_server_prepared(
        &server,
        UiRequestScope::Connection,
        UiRequestKind::ReadOnly,
        "successor UI request",
        move |_, _| {
            assert_eq!(*prepared_after.lock().unwrap(), vec![1, 2]);
            |_| async { Ok(3_u32) }
        },
        move |_, _, value| {
            successor_applied.lock().unwrap().push(value.unwrap());
            Ok(())
        },
    );
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;
    assert_eq!(app.chat_widget.composer_text_with_pending(), "x");
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    )
    .await;
    assert!(applied.lock().unwrap().is_empty());
    release.send(1_u32).unwrap();
    for _ in 0..3 {
        let completion = time::timeout(
            Duration::from_secs(5),
            app.server_operations.control_rx.recv(),
        )
        .await?
        .expect("UI request completion");
        Box::pin(app.handle_server_completion(&mut tui, &mut server, completion)).await?;
    }
    assert_eq!(*applied.lock().unwrap(), vec![1, 2, 3]);

    // Cancellation stops a blocked read and suppresses a result that was
    // already delivered but has not yet been applied by the UI task.
    for already_delivered in [false, true] {
        let (release, blocked) = oneshot::channel::<u32>();
        let cancelled_applied = Arc::clone(&applied);
        let handle = app.request_server(
            &server,
            UiRequestScope::Connection,
            UiRequestKind::ReadOnly,
            "cancelled UI read",
            move |_| async move { Ok(blocked.await.expect("read released")) },
            move |_, _, value| {
                cancelled_applied.lock().unwrap().push(value.unwrap());
                Ok(())
            },
        );
        let completion = if already_delivered {
            release.send(9).unwrap();
            let completion = time::timeout(
                Duration::from_secs(5),
                app.server_operations.control_rx.recv(),
            )
            .await?
            .expect("completed read");
            handle.cancel();
            completion
        } else {
            handle.cancel();
            let completion = time::timeout(
                Duration::from_secs(5),
                app.server_operations.control_rx.recv(),
            )
            .await?
            .expect("cancelled read");
            assert!(release.is_closed(), "cancelled read future was not dropped");
            completion
        };
        Box::pin(app.handle_server_completion(&mut tui, &mut server, completion)).await?;
        assert_eq!(*applied.lock().unwrap(), vec![1, 2, 3]);
    }

    // Retiring the connection invalidates a completed request even when its
    // result has already reached the completion channel.
    let stale_applied = Arc::clone(&applied);
    app.request_server(
        &server,
        UiRequestScope::Connection,
        UiRequestKind::ReadOnly,
        "stale UI request",
        |_| async { Ok(4_u32) },
        move |_, _, value| {
            stale_applied.lock().unwrap().push(value.unwrap());
            Ok(())
        },
    );
    let stale = time::timeout(
        Duration::from_secs(5),
        app.server_operations.control_rx.recv(),
    )
    .await?
    .expect("stale UI request completion");
    app.handle_submission_event(&server, turn_submissions::SubmissionEvent::Retire)
        .await;
    Box::pin(app.handle_server_completion(&mut tui, &mut server, stale)).await?;
    assert_eq!(*applied.lock().unwrap(), vec![1, 2, 3]);
    while events.try_recv().is_ok() {}
    let rich_inputs = |text: &str| {
        vec![
            UserInput::Text {
                text: text.into(),
                text_elements: vec![
                    TextElement::new(
                        codex_protocol::user_input::ByteRange {
                            start: 0,
                            end: text.len(),
                        },
                        None,
                    )
                    .into(),
                ],
            },
            UserInput::LocalImage {
                path: "queue-image.png".into(),
                detail: None,
            },
            UserInput::Image {
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
        crate::chatwidget::UserMessageSource::Prompt,
    )
    .message;
    app.chat_widget.snapshot_local_images = false;
    app.chat_widget
        .submit_plain_user_turn_to(thread_id, rich_prompt.clone())
        .expect("submit ordinary prompt");
    let first = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::SubmitThreadOp {
                thread_id: target,
                op: op @ AppCommand::UserTurn { .. },
            } => {
                assert_eq!(target, thread_id);
                Some(op)
            }
            _ => None,
        })
        .expect("captured ordinary prompt");
    // This failed against the synchronous routing: no subsequent App event could be serviced.
    time::timeout(
        Duration::from_secs(5),
        app.submit_thread_op(&mut server, thread_id, first),
    )
    .await
    .expect("server-controlled submission blocked the App loop")?;
    time::timeout(Duration::from_secs(5), async {
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
        vec![UserInput::Text {
            text: "waiting follow-up".into(),
            text_elements: Vec::new(),
        }],
        turn_submissions::SubagentTextSource::Prompt,
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
        AppEvent::InsertHistoryCell(Box::new(PlainHistoryCell::new(vec![
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
        vec![UserInput::Text {
            text: "unsent successor".into(),
            text_elements: Vec::new(),
        }],
        turn_submissions::SubagentTextSource::Prompt,
    );
    time::pause();
    time::advance(Duration::from_secs(31)).await;
    let completion = app
        .thread_delivery
        .completion_rx
        .recv()
        .await
        .expect("uncertain receipt");
    app.handle_local_thread_completion(&mut server, completion)
        .await?;
    time::resume();
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
    assert_eq!(
        recorded_params(&requests, "turn/steer"),
        Vec::<serde_json::Value>::new()
    );
    assert_eq!(
        recorded_params(&requests, "turn/steer/cancel"),
        Vec::<serde_json::Value>::new()
    );
    assert_eq!(
        recorded_params(&requests, "thread/queue/delete"),
        Vec::<serde_json::Value>::new()
    );
    // A reconnect retires the original thread's lane even after its widget was replaced.
    app.chat_widget.apply_external_edit("original draft".into());
    for text in ["disconnected send", "queued after disconnect"] {
        app.enqueue_subagent_turn(
            &server,
            thread_id,
            None,
            rich_inputs(text),
            turn_submissions::SubagentTextSource::Prompt,
        );
    }
    time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "turn/start").len() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker submitted the reconnecting request");
    let other = Box::pin(server.start_thread(&app.config)).await?;
    let other_id = other.session.thread_id;
    app.ensure_thread_channel(other_id)
        .store
        .lock()
        .await
        .set_session(other.session, other.turns);
    Box::pin(app.select_agent_thread_and_settle(&mut tui, &mut server, other_id)).await?;
    assert_eq!(app.chat_widget.thread_id(), Some(other_id));
    app.chat_widget
        .apply_external_edit("other thread draft".into());
    Box::pin(app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::Submission(turn_submissions::SubmissionEvent::Retire),
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
        let rich = ChatWidget::submission_draft_from_inputs(
            "fixture".into(),
            &rich_inputs(expected),
            crate::chatwidget::UserMessageSource::Prompt,
        )
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
    time::timeout(Duration::from_secs(5), async {
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
    time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "turn/interrupt").len() < interrupts_before + 1 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    time::pause();
    time::advance(Duration::from_secs(31)).await;
    let control = app
        .server_operations
        .control_rx
        .recv()
        .await
        .expect("unknown interrupt receipt");
    let turn_submissions::ServerCompletion::Request(control) = control else {
        panic!("expected request completion");
    };
    app.finish_submission(
        &mut server,
        control.scope,
        control.id,
        control.epoch,
        control.outcome,
    )
    .await?;
    time::resume();
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
    time::timeout(Duration::from_secs(5), async {
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
    assert_eq!(app.chat_widget.composer_text_with_pending(), "");
    let rename_id = match app.server_operations.preview(Some(thread_id)).thread {
        turn_submissions::ThreadSubmissionPreview::Sending { waiting, .. } => waiting
            .into_iter()
            .find_map(|entry| match entry {
                turn_submissions::SubmissionPreviewEntry::Operation {
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
        turn_submissions::SubmissionEvent::Edit {
            thread_id,
            id: rename_id,
            action: PendingSteerCancelAction::Remove,
        },
    )
    .await;
    time::pause();
    time::advance(Duration::from_secs(31)).await;
    let completion = app
        .thread_delivery
        .completion_rx
        .recv()
        .await
        .expect("skills timeout");
    app.handle_local_thread_completion(&mut server, completion)
        .await?;
    time::resume();
    Box::pin(app.settle_turn_submissions(&mut server)).await?;
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
    time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "skills/list").len() < skills_before + 2 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    app.disarm_alarm(&mut server, other_id, "queued-alarm".into());
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
    app.handle_submission_event(&server, turn_submissions::SubmissionEvent::Retire)
        .await;
    assert!(
        app.chat_widget
            .begin_alarm_disarm(other_id, "after-retirement")
    );
    app.chat_widget
        .cancel_unsent_alarm_disarm("after-retirement");
    let input = serde_json::to_value(app.chat_widget.capture_thread_input_state())?;
    assert_eq!(input["input_queue"]["turn"], "Idle");
    assert_eq!(
        recorded_params(&requests, "thread/alarm/disarm"),
        Vec::<serde_json::Value>::new()
    );
    assert_eq!(
        recorded_params(&requests, "thread/compact/start"),
        Vec::<serde_json::Value>::new()
    );
    assert!(app.server_operations.is_empty());

    // Accepted consent transfers once to the existing connection owner. A withheld write
    // must survive navigation replacement, keep dependent work queued, and leave control live.
    let consent_cwd = app.config.codex_home.join("accepted-consent");
    std::fs::create_dir(&consent_cwd)?;
    let consent_plan = crate::onboarding::onboarding_screen::DirectoryTrustPreparation::new(
        &app.config,
        &app.app_server_target,
        &consent_cwd,
        crate::onboarding::DirectoryTrustOptions::default(),
    )?;
    app.agents_overview.new_session_draft = Some(Box::new(StartupDraftPump::new(
        &tui,
        crate::startup_draft::StartupDraftInitialScreen::Onboarding,
        crate::startup_draft::StartupDraftSessionAction::NewFromCommandCenter,
    )));
    let writes_before_consent = recorded_params(&requests, "config/batchWrite").len();
    app.admit_dashboard_trust(
        &server,
        agents_overview::trust::DashboardTrustAdmission::Directory {
            consent: crate::onboarding::onboarding_screen::DirectoryTrustConsent::Path(
                consent_cwd.to_path_buf(),
            ),
            preparation: agents_overview::trust::DashboardPreparation {
                config: app.config.clone(),
                remote_cwd: None,
            },
            plan: consent_plan,
        },
    );
    time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "config/batchWrite").len() == writes_before_consent {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let accepted_write = recorded_params(&requests, "config/batchWrite")
        .pop()
        .expect("accepted write");
    assert!(
        accepted_write["edits"][0]["keyPath"]
            .as_str()
            .expect("trust key")
            .contains("accepted-consent")
    );
    assert_eq!(accepted_write["edits"][0]["value"], "trusted");
    app.chat_widget.apply_external_edit("consent draft".into());
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;
    assert!(app.chat_widget.composer_text_with_pending().contains('x'));
    app.select_agent_thread(&mut tui, &mut server, thread_id)
        .await?;
    assert!(
        app.pending_navigation_admission()
            .expect("dependent selection")
            .2,
        "later selection remains queued behind accepted consent"
    );
    app.submit_thread_op(&mut server, other_id, AppCommand::ReloadUserConfig)
        .await?;
    assert_eq!(
        recorded_params(&requests, "config/batchWrite").len(),
        writes_before_consent + 1
    );
    urgent_store
        .lock()
        .await
        .set_active_turn_id("consent-control".into());
    let interrupts_before_consent = recorded_params(&requests, "turn/interrupt").len();
    app.stop_agents_overview_thread(&server, other_id);
    time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "turn/interrupt").len() == interrupts_before_consent {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    // Replacing the selection cannot cancel or re-admit the already accepted config write.
    app.supersede_pending_navigation();
    assert!(app.agents_overview.new_session_draft.is_some());
    time::pause();
    time::advance(Duration::from_secs(31)).await;
    while !app.pending_configuration_is_uncertain() {
        let completion = app
            .server_operations
            .control_rx
            .recv()
            .await
            .expect("accepted consent deadline");
        Box::pin(app.handle_server_completion(&mut tui, &mut server, completion)).await?;
    }
    time::resume();
    assert!(app.pending_configuration_is_uncertain());
    assert!(!app.pumped_configuration_ready());
    assert_eq!(
        recorded_params(&requests, "config/batchWrite").len(),
        writes_before_consent + 1,
        "unknown accepted consent is never replayed and cannot release its queued config successor"
    );
    assert!(
        app.chat_widget
            .composer_text_with_pending()
            .contains("consent draft")
    );
    app.handle_submission_event(&server, turn_submissions::SubmissionEvent::Retire)
        .await;
    assert!(app.server_operations.is_empty());
    assert!(app.agents_overview.new_session_draft.is_some());

    // Approval delivery transfers its request identity exactly once and bypasses a stalled
    // model lane/config barrier. A second UI resolution cannot manufacture a second send.
    app.submit_thread_op(
        &mut server,
        other_id,
        AppCommand::ListSkills {
            cwds: vec![app.config.cwd.to_path_buf()],
            force_reload: false,
        },
    )
    .await?;
    app.submit_thread_op(&mut server, other_id, AppCommand::ReloadUserConfig)
        .await?;
    let approval = ServerRequest::FileChangeRequestApproval {
        request_id: codex_app_server_protocol::RequestId::Integer(991),
        params: FileChangeRequestApprovalParams {
            thread_id: other_id.to_string(),
            turn_id: "response-turn".into(),
            item_id: "response-patch".into(),
            started_at_ms: 0,
            reason: None,
            grant_root: None,
        },
    };
    assert!(
        app.pending_app_server_requests
            .note_buffered_request(&app_server_requests::BufferedServerRequest::new(
                approval.clone(),
                &server.request_handle()
            ))
            .is_none()
    );
    let decision = AppCommand::PatchApproval {
        id: "response-patch".into(),
        decision: codex_app_server_protocol::FileChangeApprovalDecision::Cancel,
    };
    let responses_before = recorded_params(&requests, "server/request/response").len();
    assert!(
        app.try_resolve_app_server_request(&server, other_id, &decision)
            .await?
    );
    assert!(
        !app.try_resolve_app_server_request(&server, other_id, &decision)
            .await?
    );
    let delivery = time::timeout(
        Duration::from_secs(5),
        app.server_operations.control_rx.recv(),
    )
    .await?
    .expect("owned response-delivery receipt");
    let turn_submissions::ServerCompletion::Request(delivery) = delivery else {
        panic!("expected request completion");
    };
    app.finish_submission(
        &mut server,
        delivery.scope,
        delivery.id,
        delivery.epoch,
        delivery.outcome,
    )
    .await?;
    time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "server/request/response").len() == responses_before {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(
        recorded_params(&requests, "server/request/response").len(),
        responses_before + 1
    );
    app.handle_submission_event(&server, turn_submissions::SubmissionEvent::Retire)
        .await;
    assert!(app.server_operations.is_empty());
    // Realtime preparation owns no signaling effect yet. Retiring its read-only stage
    // releases the matching local helper lease and cannot later start a stale attempt.
    let (attempt_id, input_generation) =
        crate::chatwidget::prepare_voice_for_thread(&mut app.chat_widget, other_id);
    app.submit_thread_op(
        &mut server,
        other_id,
        AppCommand::RealtimeConversationStart {
            thread_id: other_id,
            attempt_id,
            input_generation,
            offer_sdp: String::from("v=0\\r\\n").into(),
        },
    )
    .await?;
    time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "thread/realtime/listVoices").is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    app.chat_widget
        .apply_external_edit("voice preparation draft".into());
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;
    assert!(app.chat_widget.composer_text_with_pending().contains('x'));
    app.handle_submission_event(&server, turn_submissions::SubmissionEvent::Retire)
        .await;
    assert!(!app.chat_widget.may_receive_realtime_transcripts());
    assert_eq!(
        recorded_params(&requests, "thread/realtime/start"),
        Vec::<serde_json::Value>::new()
    );
    assert_eq!(
        recorded_params(&requests, "thread/realtime/listVoices").len(),
        1
    );
    assert!(app.server_operations.is_empty());
    // Navigation owns its withheld read without replacing the visible target/draft.
    // Canceling this read-only stage cannot turn a late reply into a selection.
    let reads_before = recorded_params(&requests, "thread/read").len();
    let visible_before = app.current_displayed_thread_id();
    app.chat_widget
        .apply_external_edit("navigation draft".into());
    time::timeout(
        Duration::from_secs(5),
        app.select_agent_thread(&mut tui, &mut server, thread_id),
    )
    .await??;
    time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "thread/read").len() == reads_before {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(app.current_displayed_thread_id(), visible_before);
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;
    assert!(app.chat_widget.composer_text_with_pending().contains('x'));
    let Some(turn_submissions::NavigationPreview::Preparing { id: navigation }) =
        app.server_operations.preview(visible_before).navigation
    else {
        panic!("pending navigation remains visible and cancellable")
    };
    app.handle_submission_event(
        &server,
        turn_submissions::SubmissionEvent::CancelNavigation { id: navigation },
    )
    .await;
    assert!(app.server_operations.is_empty());
    assert_eq!(app.current_displayed_thread_id(), visible_before);
    assert!(
        app.chat_widget
            .composer_text_with_pending()
            .contains("navigation draft")
    );
    assert_eq!(
        recorded_params(&requests, "thread/read").len(),
        reads_before + 1
    );
    // Selecting the already-visible target is also a new navigation intent:
    // its fast path must retire the previous selection, not let it win later.
    app.select_agent_thread(&mut tui, &mut server, thread_id)
        .await?;
    app.select_agent_thread(
        &mut tui,
        &mut server,
        visible_before.expect("original visible conversation"),
    )
    .await?;
    assert!(app.server_operations.is_empty());
    assert_eq!(app.current_displayed_thread_id(), visible_before);
    // A real resource-producing resume is retained after cancellation. The same target
    // cannot reacquire, but an unrelated target must still dispatch its liveness read.
    // Admission order intentionally exceeds epoch: these are different coordinates.
    // Resume owns trust metadata before acquisition; a withheld lookup must leave the
    // old conversation/draft usable, and cancellation must not dispatch a resume.
    let config_reads_before_resume = recorded_params(&requests, "config/read").len();
    let resume_cwd_override = app
        .harness_overrides
        .cwd
        .replace(app.config.cwd.to_path_buf());
    let resume_draft = recovered_message(&mut app);
    let backend_id = server.backend_id();
    app.resume_target_session(
        &mut tui,
        &mut server,
        SessionTarget {
            backend_id,
            path: None,
            thread_id,
            cwd: Some(app.config.cwd.to_path_buf()),
            history_mode: None,
        },
    )
    .await?;
    Box::pin(time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "config/read").len() == config_reads_before_resume {
            tokio::select! {
                completion = app.server_operations.control_rx.recv() => {
                    Box::pin(app.handle_server_completion(&mut tui, &mut server, completion.expect("owned resume metadata receipt"))).await?;
                }
                () = tokio::task::yield_now() => {}
            }
        }
        Ok::<_, color_eyre::Report>(())
    })).await??;
    assert_eq!(app.current_displayed_thread_id(), visible_before);
    assert_eq!(recovered_message(&mut app), resume_draft);
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;
    assert!(app.chat_widget.composer_text_with_pending().contains('x'));
    let pending = match app.server_operations.preview(visible_before).navigation {
        Some(turn_submissions::NavigationPreview::Preparing { id }) => id,
        preview => panic!("expected owned resume metadata: {preview:?}"),
    };
    app.handle_submission_event(
        &server,
        turn_submissions::SubmissionEvent::CancelNavigation { id: pending },
    )
    .await;
    app.harness_overrides.cwd = resume_cwd_override;
    assert!(app.server_operations.is_empty());
    assert_eq!(
        recorded_params(&requests, "thread/resume"),
        Vec::<serde_json::Value>::new()
    );

    let acquisition_x = Box::pin(server.start_thread(&app.config))
        .await?
        .session
        .thread_id;
    let acquisition_y = Box::pin(server.start_thread(&app.config))
        .await?
        .session
        .thread_id;
    let resumes_for = |id: ThreadId| {
        recorded_params(&requests, "thread/resume")
            .into_iter()
            .filter(|params| params["threadId"] == id.to_string())
            .count()
    };
    app.select_agent_thread(&mut tui, &mut server, acquisition_x)
        .await?;
    let (order, epoch, _) = app
        .pending_navigation_admission()
        .expect("admitted acquisition");
    assert!(order > epoch);
    Box::pin(time::timeout(Duration::from_secs(5), async {
        while resumes_for(acquisition_x) == 0 {
            tokio::select! {
                completion = app.server_operations.control_rx.recv() => {
                    Box::pin(app.handle_server_completion(&mut tui, &mut server, completion.expect("liveness receipt"))).await?;
                }
                () = tokio::task::yield_now() => {}
            }
        }
        Ok::<_, color_eyre::Report>(())
    }))
    .await??;
    let Some(turn_submissions::NavigationPreview::Preparing { id: pending }) =
        app.server_operations.preview(visible_before).navigation
    else {
        panic!("running acquisition remains cancellable")
    };
    app.handle_submission_event(
        &server,
        turn_submissions::SubmissionEvent::CancelNavigation { id: pending },
    )
    .await;
    assert_eq!(app.current_displayed_thread_id(), visible_before);
    app.select_agent_thread(&mut tui, &mut server, acquisition_y)
        .await?;
    Box::pin(time::timeout(Duration::from_secs(5), async {
        while resumes_for(acquisition_y) == 0 {
            tokio::select! {
                completion = app.server_operations.control_rx.recv() => {
                    Box::pin(app.handle_server_completion(&mut tui, &mut server, completion.expect("unrelated liveness receipt"))).await?;
                }
                () = tokio::task::yield_now() => {}
            }
        }
        Ok::<_, color_eyre::Report>(())
    }))
    .await??;
    assert_eq!(
        recorded_params(&requests, "thread/read")
            .iter()
            .filter(|params| params["threadId"] == acquisition_y.to_string())
            .count(),
        1
    );
    app.select_agent_thread(&mut tui, &mut server, acquisition_x)
        .await?;
    let (order, epoch, queued) = app
        .pending_navigation_admission()
        .expect("same-target successor remains admitted");
    assert!(order > epoch);
    assert!(
        queued,
        "retired same-target acquisition must fence dispatch, not merely await worker polling"
    );
    assert_eq!(resumes_for(acquisition_x), 1);
    assert_eq!(
        recorded_params(&requests, "thread/read")
            .iter()
            .filter(|params| params["threadId"] == acquisition_x.to_string())
            .count(),
        1
    );
    assert!(matches!(
        app.server_operations.preview(visible_before).navigation,
        Some(turn_submissions::NavigationPreview::Preparing { .. })
    ));
    // Canceling a fresh defaults read recovers its never-admitted rich input, independently
    // of the older resource owners and without replacing text typed after admission.
    app.chat_widget.apply_external_edit("newer draft".into());
    let configs_before = recorded_params(&requests, "config/read").len();
    app.start_fresh_session(&mut tui, &mut server, None, Some(rich_prompt.clone()), None)
        .await;
    time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "config/read").len() == configs_before {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let Some(turn_submissions::NavigationPreview::Preparing { id: pending }) =
        app.server_operations.preview(visible_before).navigation
    else {
        panic!("fresh defaults remain cancellable")
    };
    app.handle_submission_event(
        &server,
        turn_submissions::SubmissionEvent::CancelNavigation { id: pending },
    )
    .await;
    let recovered = recovered_message(&mut app);
    assert!(recovered.text.contains("stalled ordinary prompt"));
    assert!(recovered.text.contains("newer draft"));
    assert_eq!(recovered.remote_image_urls, rich_prompt.remote_image_urls);
    assert_eq!(recovered.local_images.len(), rich_prompt.local_images.len());
    assert_eq!(
        recovered.local_images[0].path,
        rich_prompt.local_images[0].path
    );
    assert_eq!(app.current_displayed_thread_id(), visible_before);
    assert_eq!(recorded_params(&requests, "turn/start").len(), 2);
    assert_eq!(resumes_for(acquisition_x), 1);
    assert_eq!(resumes_for(acquisition_y), 1);

    // A real fork acquisition also retains its original resource-producing future after
    // cancellation; typing remains live and the source draft/lease cannot be replaced late.
    server.thread_inject_items(other_id, vec![serde_json::from_value(serde_json::json!({
        "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "fork source" }]
    }))?]).await?;
    let forks_before = recorded_params(&requests, "thread/fork").len();
    let source_draft = recovered_message(&mut app);
    app.fork_current_thread(&mut tui, &mut server, None, Vec::new(), false);
    Box::pin(time::timeout(Duration::from_secs(5), async {
        while recorded_params(&requests, "thread/fork").len() == forks_before {
            tokio::select! {
                completion = app.thread_delivery.completion_rx.recv() => {
                    app.handle_local_thread_completion(&mut server, completion.expect("fork settings snapshot receipt")).await?;
                }
                completion = app.server_operations.control_rx.recv() => {
                    Box::pin(app.handle_server_completion(&mut tui, &mut server, completion.expect("owned fork preparation"))).await?;
                }
                () = tokio::task::yield_now() => {}
            }
        }
        Ok::<_, color_eyre::Report>(())
    })).await??;
    assert!(app.chat_widget.fork_in_progress);
    assert_eq!(app.current_displayed_thread_id(), Some(other_id));
    assert_eq!(recovered_message(&mut app), source_draft);
    let text_before_fork_key = app.chat_widget.composer_text_with_pending();
    app.handle_key_event(
        &mut tui,
        &mut server,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;
    assert_ne!(
        app.chat_widget.composer_text_with_pending(),
        text_before_fork_key
    );
    assert_eq!(
        app.chat_widget.composer_text_with_pending().len(),
        text_before_fork_key.len() + 1
    );
    assert!(render_bottom_popup(&app.chat_widget, 200).contains("newer draft"));
    let pending = match app.server_operations.preview(Some(other_id)).navigation {
        Some(turn_submissions::NavigationPreview::Preparing { id }) => id,
        preview => panic!("fork acquisition remains owned: {preview:?}"),
    };
    app.handle_submission_event(
        &server,
        turn_submissions::SubmissionEvent::CancelNavigation { id: pending },
    )
    .await;
    assert!(!app.chat_widget.fork_in_progress);
    assert_eq!(app.current_displayed_thread_id(), Some(other_id));
    assert!(app.chat_widget.composer_text_with_pending().contains('x'));
    assert_eq!(
        recorded_params(&requests, "thread/fork").len(),
        forks_before + 1
    );
    // Partial detachment changes local attachment truth only for confirmed targets.
    // The recorder rejects the original thread's unsubscribe, but confirms the visible one.
    // A connection barrier keeps this side turn definitively unsent. Confirmed closure must
    // recover it before removing the side, rather than promoting an unattended turn afterward.
    let closing_side = Box::pin(server.start_thread(&app.config))
        .await?
        .session
        .thread_id;
    app.ensure_thread_channel(closing_side).mark_live();
    app.side_threads
        .insert(closing_side, SideThreadState::new(other_id));
    app.submit_thread_op(&mut server, other_id, AppCommand::ReloadUserConfig)
        .await?;
    app.enqueue_subagent_turn(
        &server,
        closing_side,
        None,
        vec![UserInput::Text {
            text: "unsent closed-side draft".into(),
            text_elements: Vec::new(),
        }],
        turn_submissions::SubagentTextSource::Prompt,
    );
    assert_eq!(recorded_params(&requests, "turn/start").len(), 2);
    let draft_before_detach = recovered_message(&mut app);
    assert!(matches!(
        app.handle_exit_mode(&mut server, ExitMode::ShutdownFirst)
            .await,
        AppRunControl::Continue
    ));
    Box::pin(time::timeout(Duration::from_secs(5), async {
        while app.thread_event_channels[&other_id].attachment() == ThreadEventAttachment::Live {
            let completion = app
                .server_operations
                .control_rx
                .recv()
                .await
                .expect("owned partial-detach receipt");
            Box::pin(app.handle_server_completion(&mut tui, &mut server, completion)).await?;
        }
        Ok::<_, color_eyre::Report>(())
    }))
    .await??;
    assert_eq!(
        app.thread_event_channels[&other_id].attachment(),
        ThreadEventAttachment::ReplayOnly
    );
    assert_eq!(
        app.thread_event_channels[&thread_id].attachment(),
        ThreadEventAttachment::Live
    );
    assert!(
        app.thread_unavailable(other_id),
        "attachment gate also holds for embedded frontends"
    );
    assert_eq!(app.current_displayed_thread_id(), Some(other_id));
    let recovered_after_detach = recovered_message(&mut app);
    assert!(
        recovered_after_detach
            .text
            .contains(&draft_before_detach.text)
    );
    assert!(
        recovered_after_detach
            .text
            .contains("unsent closed-side draft")
    );
    assert_eq!(
        recovered_after_detach.remote_image_urls,
        draft_before_detach.remote_image_urls
    );
    assert_eq!(
        recovered_after_detach.local_images,
        draft_before_detach.local_images
    );
    assert!(app.abandoned_side_threads.contains(&closing_side));
    assert!(!app.thread_event_channels.contains_key(&closing_side));
    assert!(app.thread_unavailable(closing_side));
    assert_eq!(recorded_params(&requests, "turn/start").len(), 2);
    assert!(!app.chat_widget.maybe_send_next_queued_input());
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}
