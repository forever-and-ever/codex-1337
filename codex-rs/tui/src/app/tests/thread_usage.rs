use super::*;
use crate::chatwidget::ThreadUsageOutcome;
use codex_app_server_protocol::ThreadUsage;
use codex_protocol::account::PlanType;
use pretty_assertions::assert_eq;
use ratatui::layout::Rect;

#[tokio::test]
async fn status_delivery_waits_for_usage_across_history_and_view_changes() -> Result<()> {
    for scenario in [
        "plain",
        "intervening",
        "overlay",
        "resize",
        "failure",
        "account-change",
    ] {
        let (mut app, mut events, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        app.chat_widget.handle_thread_session(test_thread_session(
            thread_id,
            test_path_buf("/tmp/project"),
        ));
        app.account_ui.displayed = Some((
            thread_id,
            codex_app_server_protocol::GetAccountResponse {
                account_id: None,
                account: None,
                requires_openai_auth: true,
            },
        ));
        app.chat_widget.update_account_state(
            /*status_account_display*/ None,
            Some(PlanType::Business),
            /*has_chatgpt_account*/ false,
            /*has_codex_backend_auth*/ true,
        );
        while events.try_recv().is_ok() {}
        let mut server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        tui.terminal.set_viewport_area(Rect::new(
            /*x*/ 0, /*y*/ 21, /*width*/ 80, /*height*/ 3,
        ));
        app.chat_widget.add_status_output(
            /*refreshing_rate_limits*/ false, /*request_id*/ None,
        );
        let request_id = match events.try_recv() {
            Ok(AppEvent::RefreshThreadUsage {
                thread_id: requested,
                request_id,
            }) if requested == thread_id => request_id,
            event => panic!("expected a usage request before any status card: {event:?}"),
        };
        assert!(events.try_recv().is_err());
        match scenario {
            "intervening" => app.insert_history_cell(
                &mut tui,
                Box::new(PlainHistoryCell::new(vec![Line::from(
                    "intervening output",
                )])),
            ),
            "overlay" => {
                tui.set_alt_screen_enabled(/*enabled*/ false);
                app.open_transcript_overlay(&mut tui);
            }
            "resize" => {
                let size = Size::new(/*width*/ 52, /*height*/ 24);
                tui.terminal.last_known_screen_size = size;
                app.reflow_transcript_now(&mut tui, size.into())?;
            }
            "account-change" => app.chat_widget.update_account_state(
                /*status_account_display*/ None, /*plan_type*/ None,
                /*has_chatgpt_account*/ false, /*has_codex_backend_auth*/ false,
            ),
            "plain" | "failure" => {}
            _ => unreachable!(),
        }
        app.handle_event(
            &mut tui,
            &mut server,
            AppEvent::ThreadUsageLoaded {
                thread_id,
                request_id,
                result: if scenario == "failure" {
                    Err("usage request timed out".to_string())
                } else {
                    Ok(ThreadUsageOutcome::Available(ThreadUsage {
                        thread_id: thread_id.to_string(),
                        estimated_usage_credits_micros: 50_000_000,
                        estimated_usage_usd_micros: Some(1_820_000),
                        groups: Vec::new(),
                    }))
                },
            },
        )
        .await?;
        while let Ok(event) = events.try_recv() {
            app.handle_event(&mut tui, &mut server, event).await?;
        }
        if scenario == "overlay" {
            app.close_transcript_overlay(&mut tui);
        }
        let history: Vec<_> = app
            .transcript_cells
            .iter()
            .map(|cell| lines_to_single_string(&cell.display_lines(/*width*/ 90)))
            .collect();
        assert_eq!(
            history
                .iter()
                .filter(|text| text.contains("50 credits"))
                .count(),
            usize::from(!matches!(scenario, "failure" | "account-change")),
            "{scenario}"
        );
        if scenario == "failure" {
            assert!(
                history
                    .iter()
                    .any(|text| text.contains("Could not refresh thread usage"))
            );
        }
        server.shutdown().await?;
    }
    Ok(())
}
