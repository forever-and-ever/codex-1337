use super::*;
use crate::chatwidget::UserMessage;
use codex_app_server_protocol::CodexErrorInfo;
use codex_app_server_protocol::ErrorNotification;
use codex_app_server_protocol::ThreadGoalStatus;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn turn_start_failure_is_shown_without_exiting() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let thread_id = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000")?;
    app.active_thread_id = Some(thread_id);
    app.chat_widget
        .handle_thread_session(test_thread_session(thread_id, app.config.cwd.to_path_buf()));
    while app_event_rx.try_recv().is_ok() {}

    app.chat_widget
        .restore_user_message_to_composer(UserMessage::from("hello"));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let op = next_user_turn_op(&mut op_rx);
    while let Ok(event) = app_event_rx.try_recv() {
        app.handle_event(&mut tui, &mut app_server, event).await?;
    }

    let control = app
        .handle_event(
            &mut tui,
            &mut app_server,
            AppEvent::SubmitThreadOp { thread_id, op },
        )
        .await?;

    assert!(matches!(control, AppRunControl::Continue));
    app.settle_turn_submissions(&mut app_server).await?;
    let error_cell = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(cell),
            _ => None,
        })
        .expect("turn/start failure should be added to history");
    let transcript = app
        .transcript_cells
        .iter()
        .map(|cell| lines_to_single_string(&cell.display_lines(/*width*/ 80)))
        .chain(std::iter::once(lines_to_single_string(
            &error_cell.display_lines(/*width*/ 80),
        )))
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(transcript, @r"
    › hello
    ■ Failed to start turn: turn/start failed in TUI: turn/start failed: thread not found: 123e4567-e89b-12d3-a456-426614174000 (code -32600)
    ");

    use crate::chatwidget::UserMessageSource;

    app.chat_widget.set_parent_owned_thread();
    app.chat_widget.apply_external_edit("child draft".into());
    let question_notification =
        ServerNotification::ItemCompleted(codex_app_server_protocol::ItemCompletedNotification {
            thread_id: thread_id.to_string(),
            turn_id: "questions-turn".into(),
            content_token_count: None,
            completed_at_ms: 0,
            item: serde_json::from_value(serde_json::json!({
                "type": "agentMessage",
                "id": "child-questions",
                "text": "",
                "questions": [
                    { "title": "First?", "options": ["Retained child answer"] },
                    { "title": "Second?", "options": ["Retained child answer"] }
                ]
            }))?,
        });
    app.chat_widget
        .handle_server_notification(question_notification.clone(), None);
    // Reselecting a thread leaves its prior inactive snapshot in the active store.
    app.store_active_thread_receiver().await;
    app.set_thread_active(thread_id, true).await;
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
    for _ in 0..2 {
        render_bottom_popup(&app.chat_widget, 80);
        app.chat_widget
            .handle_key_event(KeyEvent::from(KeyCode::Enter));
    }
    let answers = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .filter(|event| matches!(event, AppEvent::SubagentTextInput { .. }))
        .collect::<Vec<_>>();
    assert_eq!(answers.len(), 2);
    let replies = answers
        .iter()
        .map(|event| {
            let AppEvent::SubagentTextInput { input, .. } = event else {
                unreachable!()
            };
            let [codex_app_server_protocol::UserInput::Text { text, .. }] = input.as_slice() else {
                unreachable!()
            };
            text.clone()
        })
        .collect::<Vec<_>>();

    let mut answers = answers.into_iter();
    app.handle_event(&mut tui, &mut app_server, answers.next().unwrap())
        .await?;

    // Close the selected source before the second AppEvent is admitted.
    app.discard_thread_local_state(thread_id).await;
    let unrelated = app_server.start_thread(&app.config).await?;
    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        unrelated,
        super::super::session_lifecycle::ThreadAttachPresentation::Fresh,
        None,
    )
    .await?;
    app.chat_widget
        .apply_external_edit("unrelated draft".into());
    let unrelated_input = app.chat_widget.capture_thread_input_state().unwrap();
    // This recreated channel shares the thread ID, not the active operation's Arc.
    app.handle_event(&mut tui, &mut app_server, answers.next().unwrap())
        .await?;
    assert!(answers.next().is_none());
    let crate::app::turn_submissions::ThreadSubmissionPreview::Sending { waiting, .. } =
        app.server_operations.thread_preview(Some(thread_id))
    else {
        panic!("child answer delivery is active");
    };
    assert_eq!(
        waiting.len(),
        1,
        "child answers use one serialized delivery lane"
    );
    // Remove the newly created channel before its rejection callback runs as well.
    app.discard_thread_local_state(thread_id).await;
    app.settle_turn_submissions(&mut app_server).await?;
    assert_eq!(
        app.chat_widget.capture_thread_input_state().unwrap(),
        unrelated_input
    );

    let mut recovered = app
        .agents_overview
        .input_states
        .remove(&thread_id)
        .expect("closed source retains its own rejected answers");
    let saved = serde_json::to_value(&recovered)?;
    assert_eq!(saved["questions"]["pending"].as_array().unwrap().len(), 0);
    app.chat_widget
        .handle_thread_session(test_thread_session(thread_id, app.config.cwd.to_path_buf()));
    app.chat_widget.set_parent_owned_thread();
    app.chat_widget.restore_thread_input_state(
        Some(recovered.clone()),
        crate::chatwidget::ThreadInputStateRestoreMode {
            preserve_in_flight_turn: true,
        },
    );
    let queued = app.chat_widget.queued_user_message_texts();
    assert_eq!(&queued[..2], &replies);
    for reply in &replies {
        assert_eq!(queued.iter().filter(|text| *text == reply).count(), 1);
    }
    assert!(
        !app.chat_widget.maybe_send_next_queued_input(),
        "recovery never autosends"
    );
    app.chat_widget
        .handle_server_notification(question_notification, None);
    let replayed = serde_json::to_value(app.chat_widget.capture_thread_input_state())?;
    assert_eq!(
        replayed["questions"]["pending"].as_array().unwrap().len(),
        0
    );

    recovered.input_queue.recovered_queue = false;
    app.chat_widget.restore_thread_input_state(
        Some(recovered),
        crate::chatwidget::ThreadInputStateRestoreMode {
            preserve_in_flight_turn: true,
        },
    );
    while app_event_rx.try_recv().is_ok() {}
    assert!(app.chat_widget.maybe_send_next_queued_input());
    let retry = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find(|event| matches!(event, AppEvent::SubagentTextInput { .. }))
        .expect("recovered child answer retries through its delivery owner");
    let AppEvent::SubagentTextInput {
        thread_id: target,
        source,
        input,
        ..
    } = &retry
    else {
        unreachable!()
    };
    assert_eq!(*target, thread_id);
    assert_eq!(source.message_source(), UserMessageSource::QuestionAnswer);
    assert_eq!(
        crate::async_question_reply::parse_input(input).unwrap()[0].question_item_id,
        serde_json::json!(["request_user_input_async", "child-questions", 0]).to_string()
    );
    app.handle_event(&mut tui, &mut app_server, retry).await?;
    app.settle_turn_submissions(&mut app_server).await?;

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn misalignment_policy_blocks_queued_turns_and_goal_resumption() -> Result<()> {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let checkout = tempfile::tempdir()?;
    app.config.cwd = AbsolutePathBuf::from_absolute_path(checkout.path())?;
    for args in [
        vec!["init", "--quiet"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(checkout.path())
                .status()?
                .success()
        );
    }
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let started = app_server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    let session = started.session.clone();
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    crate::test_support::thread_goal_set(
        &app_server,
        thread_id,
        Some("Do not restart this stopped goal".to_string()),
        Some(ThreadGoalStatus::Blocked),
        /*token_budget*/ None,
    )
    .await?;
    app.chat_widget
        .restore_user_message_to_composer("queued before terminal failure".into());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let queued_turn = next_user_turn_op(&mut op_rx);
    let failed_turn = Turn {
        error: Some(AppServerTurnError {
            misalignment: None,
            message: "misalignment policy violation".to_string(),
            codex_error_info: Some(CodexErrorInfo::MisalignmentPolicyViolation),
            additional_details: None,
        }),
        ..test_turn("turn-1", TurnStatus::Failed, Vec::new())
    };
    app.enqueue_primary_thread_session(session, vec![failed_turn])
        .await?;
    let precaution = render_bottom_popup(&app.chat_widget, /*width*/ 80);
    let AppServerRequestId::Integer(next_request_id) = app_server.next_request_id() else {
        unreachable!("embedded app-server request IDs are integers");
    };

    app.maybe_prompt_resume_paused_goal_after_resume(&mut app_server, thread_id)
        .await;
    for event in [
        AppEvent::SubmitThreadOp {
            thread_id,
            op: queued_turn,
        },
        AppEvent::CodexOp(crate::app_command::AppCommand::ExecApproval {
            id: "queued-approval".to_string(),
            turn_id: None,
            decision: codex_app_server_protocol::CommandExecutionApprovalDecision::Accept,
        }),
        AppEvent::SetThreadGoalStatus {
            thread_id,
            status: ThreadGoalStatus::Active,
        },
        AppEvent::SetThreadGoalDraft {
            thread_id,
            draft: crate::goal_files::GoalDraft {
                objective: "Do not replace this stopped goal".to_string(),
                ..Default::default()
            },
            mode: crate::app_event::ThreadGoalSetMode::ReplaceExisting,
        },
        AppEvent::StartSide {
            parent_thread_id: thread_id,
            user_message: Some("Do not fork this stopped thread".into()),
            mode: crate::app_event::SideStartMode::Reuse,
        },
        AppEvent::ForkCurrentSession { name: None },
        AppEvent::OpenAgentPicker,
        AppEvent::SelectAgentThread(thread_id),
    ] {
        app.handle_event_and_settle_navigation(&mut tui, &mut app_server, event)
            .await?;
    }

    assert_eq!(
        app_server.next_request_id(),
        AppServerRequestId::Integer(next_request_id + 1)
    );
    assert_eq!(
        render_bottom_popup(&app.chat_widget, /*width*/ 80),
        precaution
    );
    assert_eq!(
        crate::test_support::thread_goal_get(&app_server, thread_id)
            .await?
            .goal
            .unwrap()
            .status,
        ThreadGoalStatus::Blocked
    );

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn misalignment_policy_in_parent_stops_active_side_conversation() -> Result<()> {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    app.active_thread_id = Some(side_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));
    app.chat_widget.handle_thread_session(test_thread_session(
        side_thread_id,
        app.config.cwd.to_path_buf(),
    ));
    app.chat_widget
        .restore_user_message_to_composer("pending side turn".into());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    while op_rx.try_recv().is_ok() {}

    app.enqueue_thread_notification(
        parent_thread_id,
        ServerNotification::Error(ErrorNotification {
            error: AppServerTurnError {
                misalignment: None,
                message: "misalignment policy violation".to_string(),
                codex_error_info: Some(CodexErrorInfo::MisalignmentPolicyViolation),
                additional_details: None,
            },
            will_retry: false,
            thread_id: parent_thread_id.to_string(),
            turn_id: "parent-turn".to_string(),
        }),
    )
    .await?;

    assert!(app.chat_widget.has_misalignment_policy_violation());
    assert!(
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
            .contains("Chat stopped as a precaution")
    );
    assert_matches!(op_rx.try_recv(), Ok(Op::Interrupt));

    Ok(())
}

#[tokio::test]
async fn launch_permissions_preserve_server_owned_constrained_profile() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    app.harness_overrides.sandbox_mode =
        Some(codex_protocol::config_types::SandboxMode::DangerFullAccess);
    app.config
        .permissions
        .set_permission_profile(PermissionProfile::Disabled)?;
    app.remember_launch_permissions();
    let mut profile = PermissionProfile::workspace_write();
    let PermissionProfile::Managed {
        file_system: ManagedFileSystemPermissions::Restricted { entries, .. },
        ..
    } = &mut profile
    else {
        unreachable!("workspace-write uses managed permissions");
    };
    entries.push(FileSystemSandboxEntry {
        path: FileSystemPath::GlobPattern {
            pattern: "**/*.secret".into(),
        },
        access: FileSystemAccessMode::Deny,
        missing_path_behavior: None,
    });
    app.config.permissions.set_permission_profile(profile)?;
    app.harness_overrides.sandbox_mode =
        Some(codex_protocol::config_types::SandboxMode::WorkspaceWrite);
    app.remember_launch_permissions();
    assert_eq!(app.runtime_permission_profile_override, None);
    Ok(())
}

#[tokio::test]
async fn unsupported_legacy_permissions_are_shown_without_exiting() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);
    app.chat_widget
        .handle_thread_session(test_thread_session(thread_id, app.config.cwd.to_path_buf()));

    let extra_root = codex_utils_absolute_path::AbsolutePathBuf::resolve_path_against_base(
        "extra",
        app.config.cwd.as_path(),
    );
    let permission_profile = PermissionProfile::Managed {
        network: NetworkSandboxPolicy::Restricted,
        file_system: ManagedFileSystemPermissions::Restricted {
            entries: vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Read,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Path {
                        path: extra_root.into(),
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
            ],
            glob_scan_max_depth: None,
        },
    };
    app.config
        .permissions
        .set_permission_profile(permission_profile.clone())?;
    app.chat_widget.set_permission_profile_with_active_profile(
        permission_profile,
        /*active_permission_profile*/ None,
    )?;
    app.runtime_permission_profile_override =
        Some(RuntimePermissionProfileOverride::from_config(&app.config));
    while app_event_rx.try_recv().is_ok() {}

    app.chat_widget
        .restore_user_message_to_composer(UserMessage::from("hello"));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let op = next_user_turn_op(&mut op_rx);
    app.chat_widget
        .restore_user_message_to_composer(UserMessage::from("queued follow-up"));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.chat_widget.has_queued_follow_up_messages());
    while let Ok(event) = app_event_rx.try_recv() {
        app.handle_event(&mut tui, &mut app_server, event).await?;
    }

    let control = app
        .handle_event(
            &mut tui,
            &mut app_server,
            AppEvent::SubmitThreadOp { thread_id, op },
        )
        .await?;

    assert!(matches!(control, AppRunControl::Continue));
    assert!(app.chat_widget.has_queued_follow_up_messages());
    let error_cell = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(cell),
            _ => None,
        })
        .expect("unsupported permissions should be added to history");
    insta::assert_snapshot!(
        lines_to_single_string(&error_cell.display_lines(/*width*/ 200)),
        @"■ Failed to start turn: the selected permission profile cannot be safely represented by the legacy app-server sandbox policy; select a named or legacy-compatible permission profile"
    );

    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::SettingsSelectionSettled,
    )
    .await?;
    assert!(app.chat_widget.has_queued_follow_up_messages());

    app.config
        .permissions
        .set_permission_profile(PermissionProfile::read_only())?;
    app.chat_widget.set_permission_profile_with_active_profile(
        PermissionProfile::read_only(),
        /*active_permission_profile*/ None,
    )?;
    app.runtime_permission_profile_override =
        Some(RuntimePermissionProfileOverride::from_config(&app.config));
    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::SettingsSelectionSettled,
    )
    .await?;
    assert!(!app.chat_widget.has_queued_follow_up_messages());

    app_server.shutdown().await?;
    Ok(())
}
