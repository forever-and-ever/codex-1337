use super::*;
use crate::chatwidget::ThreadUsageOutcome;
use assert_matches::assert_matches;
use codex_app_server_protocol::ThreadUsage;
use codex_app_server_protocol::ThreadUsageBreakdownGroup;
use codex_utils_path_uri::PathUri;

fn complete_status_limits(
    chat: &mut ChatWidget,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> String {
    let mut output = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::RefreshRateLimits {
                origin: RateLimitRefreshOrigin::StatusCommand { request_id },
            } => {
                chat.finish_status_rate_limit_refresh(request_id, vec![snapshot(/*percent*/ 20.0)])
            }
            AppEvent::InsertHistoryCell(cell) => {
                output.push(lines_to_single_string(&cell.display_lines(/*width*/ 90)));
            }
            event => panic!("unexpected status event: {event:?}"),
        }
    }
    assert!(
        !output.is_empty(),
        "status must be printed after its refreshes finish"
    );
    output.join("\n")
}

#[tokio::test]
async fn status_command_waits_for_fresh_limits_before_printing() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);

    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected rate-limit refresh request, got {other:?}"),
    };
    pretty_assertions::assert_eq!(request_id, 0);
    assert!(drain_insert_history(&mut rx).is_empty());
    chat.finish_status_rate_limit_refresh(request_id, vec![snapshot(/*percent*/ 20.0)]);
    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1);
    assert!(lines_to_single_string(&cells[0]).contains("80% left"));
}

#[tokio::test]
async fn status_command_does_not_print_cached_limits_when_refresh_fails() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.on_rate_limit_snapshot(Some(snapshot(/*percent*/ 92.0)));
    drain_insert_history(&mut rx);
    chat.dispatch_command(SlashCommand::Status);
    let first_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected rate-limit refresh request, got {other:?}"),
    };

    chat.finish_status_rate_limit_refresh(first_request_id, Vec::new());
    let refreshed = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected refreshed status output, got {other:?}"),
    };
    assert!(
        !refreshed.contains("8% left"),
        "failed refresh must not print cached limits: {refreshed}"
    );
}

#[tokio::test]
async fn status_command_renders_immediately_without_rate_limit_refresh() {
    use crate::history_cell::NotificationCell;
    use crate::history_cell::UserHistoryCell;
    use crate::prompt_order::UserPromptIdentity;
    use crate::transcript_view::TranscriptView;

    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.dispatch_command(SlashCommand::Status);

    let status = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        event => panic!("expected immediate status output, got {event:?}"),
    };
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::RefreshRateLimits { .. })),
        "non-ChatGPT sessions should not request a rate-limit refresh for /status"
    );

    // /status is an explicit inspection, not dialogue: dialogue mode shows it while hiding the
    // work around it.
    let mut cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(UserHistoryCell {
        timestamp: None,
        prompt_identity: UserPromptIdentity::default(),
        message: "Continue working".into(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
        presentation: Default::default(),
    })];
    let mut view = TranscriptView::default();
    let area = Rect::new(0, 0, 140, 80);
    let mut buffer = Buffer::empty(area);
    view.toggle_lens(&cells);
    cells.push(Arc::new(NotificationCell::new(
        "Hidden before status".into(),
    )));
    cells.push(Arc::from(status));
    cells.push(Arc::new(NotificationCell::new(
        "Hidden after status".into(),
    )));
    view.render(area, &mut buffer, &cells);
    let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(rendered.contains("/status"), "{rendered}");
    assert!(rendered.contains(&thread_id.to_string()), "{rendered}");
    assert!(!rendered.contains("Hidden before status"), "{rendered}");
    assert!(!rendered.contains("Hidden after status"), "{rendered}");
    assert!(view.dialogue_only());
}

#[tokio::test]
async fn status_command_uses_catalog_default_reasoning_when_config_empty() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    chat.config.model_reasoning_effort = None;

    chat.dispatch_command(SlashCommand::Status);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected status output, got {other:?}"),
    };
    assert!(
        rendered.contains("GPT-5.5 (reasoning medium, summaries auto)"),
        "expected /status to render the catalog default reasoning effort, got: {rendered}"
    );
}

#[tokio::test]
async fn status_command_renders_native_and_foreign_instruction_sources() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let (foreign_source, foreign_display) = if cfg!(windows) {
        (
            PathUri::parse("file:///remote/AGENTS.md").expect("POSIX instruction source"),
            "/remote/AGENTS.md",
        )
    } else {
        (
            PathUri::parse("file:///C:/remote/AGENTS.md").expect("Windows instruction source"),
            r"C:\remote\AGENTS.md",
        )
    };
    chat.instruction_source_paths = vec![
        PathUri::from_abs_path(&chat.config.cwd.join("AGENTS.md")),
        foreign_source,
    ];

    chat.dispatch_command(SlashCommand::Status);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected status output, got {other:?}"),
    };
    assert!(
        rendered.contains(&format!("AGENTS.md, {foreign_display}")),
        "expected /status to show native-relative and environment-native foreign paths, got: {rendered}"
    );
    assert!(
        !rendered.contains("Agents.md  <none>"),
        "expected /status to avoid stale <none> when app-server provided instruction sources, got: {rendered}"
    );
}

#[tokio::test]
async fn status_command_overlapping_refreshes_update_matching_cells_only() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);
    let first_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected first refresh request, got {other:?}"),
    };

    chat.dispatch_command(SlashCommand::Status);
    let second_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected second refresh request, got {other:?}"),
    };

    assert_ne!(first_request_id, second_request_id);
    assert!(drain_insert_history(&mut rx).is_empty());

    chat.finish_status_rate_limit_refresh(first_request_id, Vec::new());
    pretty_assertions::assert_eq!(chat.refreshing_status_outputs.len(), 1);

    chat.finish_status_rate_limit_refresh(second_request_id, vec![snapshot(/*percent*/ 20.0)]);
    assert!(chat.refreshing_status_outputs.is_empty());
    assert_eq!(drain_insert_history(&mut rx).len(), 2);
}

#[tokio::test]
async fn account_update_rejects_stale_status_rate_limit_snapshots() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.dispatch_command(SlashCommand::Status);
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected status refresh request, got {other:?}"),
    };

    {
        chat.set_test_backend_auth(true);
        chat.update_account_state(crate::app_server_session::SessionAccount {
            display: (true).then(|| StatusAccountDisplay::ChatGpt {
                email: None,
                plan: None,
                workspace: None,
            }),
            plan: None,
            requires_openai_auth: chat.requires_openai_auth(),
        });
    };
    chat.finish_status_rate_limit_refresh(request_id, vec![snapshot(/*percent*/ 92.0)]);

    assert!(chat.rate_limit_snapshots_by_limit_id.is_empty());
}

#[tokio::test]
async fn status_command_waits_for_usage_and_limits_without_status_line() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    let thread_id =
        ThreadId::from_string("019fc8ab-1fb2-7000-8000-000000000001").expect("valid thread id");
    chat.thread_id = Some(thread_id);
    chat.test_account_mut().plan = Some(PlanType::Business);

    chat.dispatch_command(SlashCommand::Status);

    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshThreadUsage {
            thread_id: requested_thread_id,
            request_id,
        }) => {
            assert_eq!(requested_thread_id, thread_id);
            request_id
        }
        event => panic!("expected /status to request thread usage, got {event:?}"),
    };
    assert_eq!(chat.pending_status_outputs.len(), 1);
    assert!(chat.finish_thread_usage_refresh(
        thread_id,
        request_id,
        Ok(ThreadUsageOutcome::Available(ThreadUsage {
            native_usage_usd_micros: None,
            thread_id: thread_id.to_string(),
            estimated_usage_credits_micros: 50_000_000,
            estimated_usage_usd_micros: Some(1_820_000),
            groups: vec![
                ThreadUsageBreakdownGroup {
                    model: Some("gpt-5.4".to_string()),
                    reasoning_effort: Some("high".to_string()),
                    speed: Some("fast".to_string()),
                    estimated_usage_credits_micros: 40_000_000,
                    net_new_input_tokens: Some(80),
                    cached_input_tokens: Some(20),
                    input_tokens: Some(100),
                    output_tokens: Some(40),
                    total_tokens: Some(140),
                },
                ThreadUsageBreakdownGroup {
                    model: Some("gpt-5-mini".to_string()),
                    reasoning_effort: Some("medium".to_string()),
                    speed: Some("standard".to_string()),
                    estimated_usage_credits_micros: 10_000_000,
                    net_new_input_tokens: Some(15),
                    cached_input_tokens: Some(5),
                    input_tokens: Some(20),
                    output_tokens: Some(10),
                    total_tokens: Some(30),
                },
            ],
        })),
    ));

    let rendered = normalize_snapshot_paths(complete_status_limits(&mut chat, &mut rx));
    assert!(rendered.contains("50 credits · ~$1.82"));
    assert!(rendered.contains("GPT-5.4 80%, GPT-5 Mini 20%"));
    assert!(rendered.contains("Medium 20%, High 80%"));
    assert!(rendered.contains("Fast mode 80%, Standard 20%"));
    assert!(rendered.contains("120 input (25 cached) + 50 output"));
    assert!(
        rendered.find("Limits:") < rendered.find("Thread usage:"),
        "account limits should appear above the thread usage group: {rendered}"
    );
    let thread_usage = rendered
        .lines()
        .skip_while(|line| !line.contains("Thread usage:"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_chatwidget_snapshot!("status_command_estimated_thread_usage", thread_usage);
}

#[tokio::test]
async fn status_command_omits_thread_usage_for_unsupported_plans() {
    for plan_type in [
        Some(PlanType::Enterprise),
        Some(PlanType::Pro),
        Some(PlanType::Edu),
        Some(PlanType::SelfServeBusinessUsageBased),
        Some(PlanType::Unknown),
        None,
    ] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
        set_chatgpt_auth(&mut chat);
        chat.thread_id = Some(ThreadId::new());
        chat.test_account_mut().plan = plan_type;

        chat.dispatch_command(SlashCommand::Status);

        let rendered = complete_status_limits(&mut chat, &mut rx);
        assert!(!rendered.contains("Thread usage"));
        assert!(
            !std::iter::from_fn(|| rx.try_recv().ok())
                .any(|event| matches!(event, AppEvent::RefreshThreadUsage { .. })),
            "unsupported plan {plan_type:?} must not request thread usage",
        );
    }
}

#[tokio::test]
async fn status_command_reports_usage_failure_and_can_retry() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.test_account_mut().plan = Some(PlanType::Business);

    chat.dispatch_command(SlashCommand::Status);
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshThreadUsage { request_id, .. }) => request_id,
        event => panic!("expected thread usage request, got {event:?}"),
    };
    assert!(chat.finish_thread_usage_refresh(
        thread_id,
        request_id,
        Err("thread usage temporarily unavailable".to_string()),
    ));
    assert!(complete_status_limits(&mut chat, &mut rx).contains("Could not refresh thread usage"));

    chat.dispatch_command(SlashCommand::Status);
    let retry_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshThreadUsage { request_id, .. }) => request_id,
        event => panic!("expected thread usage retry, got {event:?}"),
    };
    assert!(chat.finish_thread_usage_refresh(
        thread_id,
        retry_request_id,
        Ok(ThreadUsageOutcome::Available(ThreadUsage {
            native_usage_usd_micros: None,
            thread_id: thread_id.to_string(),
            estimated_usage_credits_micros: 50_000_000,
            estimated_usage_usd_micros: Some(1_820_000),
            groups: Vec::new(),
        })),
    ));

    assert!(complete_status_limits(&mut chat, &mut rx).contains("50 credits · ~$1.82"));
}

#[tokio::test]
async fn status_command_renders_credits_and_breakdowns_without_usd_estimate() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.test_account_mut().plan = Some(PlanType::Business);

    chat.dispatch_command(SlashCommand::Status);
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshThreadUsage { request_id, .. }) => request_id,
        event => panic!("expected thread usage request, got {event:?}"),
    };
    assert_eq!(chat.pending_status_outputs.len(), 1);
    assert!(chat.finish_thread_usage_refresh(
        thread_id,
        request_id,
        Ok(ThreadUsageOutcome::Available(ThreadUsage {
            native_usage_usd_micros: None,
            thread_id: thread_id.to_string(),
            estimated_usage_credits_micros: 50_000_000,
            estimated_usage_usd_micros: None,
            groups: vec![ThreadUsageBreakdownGroup {
                model: Some("gpt-5.4".to_string()),
                reasoning_effort: Some("high".to_string()),
                speed: Some("fast".to_string()),
                estimated_usage_credits_micros: 50_000_000,
                net_new_input_tokens: Some(80),
                cached_input_tokens: Some(20),
                input_tokens: Some(100),
                output_tokens: Some(40),
                total_tokens: Some(140),
            }],
        })),
    ));

    let rendered = complete_status_limits(&mut chat, &mut rx);
    assert!(rendered.contains("50 credits"));
    assert!(rendered.contains("GPT-5.4 100%"));
    assert!(rendered.contains("High 100%"));
    assert!(rendered.contains("Fast mode 100%"));
    assert!(rendered.contains("100 input (20 cached) + 40 output"));
    assert!(!rendered.contains("~$"));
    let thread_usage = rendered
        .lines()
        .skip_while(|line| !line.contains("Thread usage:"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_chatwidget_snapshot!(
        "status_command_estimated_thread_usage_without_usd",
        thread_usage
    );
}

#[tokio::test]
async fn status_command_drops_stale_usd_when_updated_usage_has_only_credits() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.test_account_mut().plan = Some(PlanType::Business);

    chat.dispatch_command(SlashCommand::Status);
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshThreadUsage { request_id, .. }) => request_id,
        event => panic!("expected initial thread usage request, got {event:?}"),
    };
    assert_eq!(chat.pending_status_outputs.len(), 1);
    assert!(chat.finish_thread_usage_refresh(
        thread_id,
        request_id,
        Ok(ThreadUsageOutcome::Available(ThreadUsage {
            native_usage_usd_micros: None,
            thread_id: thread_id.to_string(),
            estimated_usage_credits_micros: 40_000_000,
            estimated_usage_usd_micros: Some(1_820_000),
            groups: Vec::new(),
        })),
    ));

    assert!(complete_status_limits(&mut chat, &mut rx).contains("40 credits · ~$1.82"));
    chat.dispatch_command(SlashCommand::Status);
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshThreadUsage { request_id, .. }) => request_id,
        event => panic!("expected refreshed thread usage request, got {event:?}"),
    };
    assert_eq!(chat.pending_status_outputs.len(), 1);
    assert!(chat.finish_thread_usage_refresh(
        thread_id,
        request_id,
        Ok(ThreadUsageOutcome::Available(ThreadUsage {
            native_usage_usd_micros: None,
            thread_id: thread_id.to_string(),
            estimated_usage_credits_micros: 50_000_000,
            estimated_usage_usd_micros: None,
            groups: Vec::new(),
        })),
    ));

    let refreshed = complete_status_limits(&mut chat, &mut rx);
    assert!(refreshed.contains("50 credits"));
    assert!(!refreshed.contains("~$1.82"));
}

#[tokio::test]
async fn status_command_requests_thread_usage_for_remote_connection_metadata() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    {
        chat.set_test_backend_auth(true);
        chat.update_account_state(crate::app_server_session::SessionAccount {
            display: (false).then(|| StatusAccountDisplay::ChatGpt {
                email: None,
                plan: None,
                workspace: None,
            }),
            plan: Some(PlanType::Business),
            requires_openai_auth: chat.requires_openai_auth(),
        });
    };
    chat.remote_connection = Some(crate::status::remote_connection::RemoteConnectionStatus {
        address: "wss://remote.example.com".to_string(),
        version: "v1.0.0".to_string(),
        is_local_daemon: false,
        is_managed_daemon: false,
    });

    assert_matches!(rx.try_recv(), Ok(AppEvent::FetchModels { .. }));
    chat.dispatch_command(SlashCommand::Status);

    assert_matches!(rx.try_recv(), Ok(AppEvent::RefreshThreadUsage { .. }));
}

#[tokio::test]
async fn status_command_requests_thread_usage_with_backend_only_authentication() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    {
        chat.set_test_backend_auth(true);
        chat.update_account_state(crate::app_server_session::SessionAccount {
            display: (false).then(|| StatusAccountDisplay::ChatGpt {
                email: None,
                plan: None,
                workspace: None,
            }),
            plan: Some(PlanType::Business),
            requires_openai_auth: chat.requires_openai_auth(),
        });
    };
    assert!(!chat.has_chatgpt_account());
    assert!(chat.has_codex_backend_auth());
    assert_matches!(rx.try_recv(), Ok(AppEvent::FetchModels { .. }));
    chat.dispatch_command(SlashCommand::Status);

    assert_matches!(rx.try_recv(), Ok(AppEvent::RefreshThreadUsage { .. }));
}

#[tokio::test]
async fn status_command_is_cancelled_when_account_changes_during_usage_refresh() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.test_account_mut().plan = Some(PlanType::Business);

    chat.dispatch_command(SlashCommand::Status);
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshThreadUsage { request_id, .. }) => request_id,
        event => panic!("expected thread usage request, got {event:?}"),
    };
    drain_insert_history(&mut rx);

    {
        chat.set_test_backend_auth(false);
        chat.update_account_state(crate::app_server_session::SessionAccount {
            display: (false).then(|| StatusAccountDisplay::ChatGpt {
                email: None,
                plan: None,
                workspace: None,
            }),
            plan: None,
            requires_openai_auth: chat.requires_openai_auth(),
        });
    };

    assert!(!chat.finish_thread_usage_refresh(
        thread_id,
        request_id,
        Ok(ThreadUsageOutcome::Available(ThreadUsage {
            native_usage_usd_micros: None,
            thread_id: thread_id.to_string(),
            estimated_usage_credits_micros: 50_000_000,
            estimated_usage_usd_micros: Some(1_820_000),
            groups: Vec::new(),
        })),
    ));
    assert!(drain_insert_history(&mut rx).is_empty());
}

#[tokio::test]
async fn status_command_is_cancelled_when_thread_changes_during_usage_refresh() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.test_account_mut().plan = Some(PlanType::Business);

    chat.dispatch_command(SlashCommand::Status);
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshThreadUsage { request_id, .. }) => request_id,
        event => panic!("expected thread usage request, got {event:?}"),
    };
    drain_insert_history(&mut rx);

    chat.thread_id = Some(ThreadId::new());
    chat.clear_thread_usage_state();

    assert!(!chat.finish_thread_usage_refresh(
        thread_id,
        request_id,
        Ok(ThreadUsageOutcome::Available(ThreadUsage {
            native_usage_usd_micros: None,
            thread_id: thread_id.to_string(),
            estimated_usage_credits_micros: 50_000_000,
            estimated_usage_usd_micros: Some(1_820_000),
            groups: Vec::new(),
        })),
    ));
    assert!(drain_insert_history(&mut rx).is_empty());
}
