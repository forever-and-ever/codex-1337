use super::*;
use crate::app::test_support::make_test_app;
use crate::chatwidget::tests::helpers::render_bottom_popup;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use crate::status::remote_connection::RemoteConnectionStatus;
use crossterm::event::KeyCode;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn daemon_version_notice_links_to_status() {
    let mut app = make_test_app().await;
    app.app_server_target = AppServerTarget::LocalDaemon {
        allow_embedded_fallback: true,
        endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
            socket_path: AbsolutePathBuf::relative_to_current_dir("codex.sock").unwrap(),
        },
    };
    let view = app.agents_overview_view(Vec::new(), /*selected_thread_id*/ None);
    app.chat_widget.show_bottom_pane_view(Box::new(view));
    let mut notices = Vec::new();
    for (client, server, comparison) in [
        ("0.155.0-alpha.23", "0.155.0-alpha.22", "older than"),
        ("0.155.0-alpha.23", "0.156.0", "different from"),
        ("0.0.0", "0.156.0", "different from"),
    ] {
        app.local_settings.tui.show_server_version_notice = true;
        assert_eq!(
            app.initialize_server_version_notice(client, Some(server)),
            Some(format!(
                "A background Codex service is running v{server}, {comparison} your Codex CLI v{client}."
            ))
        );
        let overview = render_bottom_popup(&app.chat_widget, /*width*/ 100);
        notices.push(
            overview
                .lines()
                .find(|line| line.contains("Service v"))
                .unwrap()
                .trim()
                .to_string(),
        );

        app.local_settings.tui.show_server_version_notice = false;
        assert_eq!(
            app.initialize_server_version_notice(client, Some(server)),
            None
        );
        assert_eq!(
            app.agents_overview
                .view_state
                .lock()
                .unwrap()
                .server_version_notice,
            None
        );
    }
    insta::assert_snapshot!(notices.join("\n"), @"
    Service v0.155.0-alpha.22 < Codex CLI v0.155.0-alpha.23 · /daemon
    Service v0.156.0 ≠ Codex CLI v0.155.0-alpha.23 · /daemon
    Service v0.156.0 ≠ Codex CLI v0.0.0 · /daemon
    ");
}

#[tokio::test]
async fn daemon_menu_reports_local_status_without_update_actions() {
    let mut app = make_test_app().await;
    let (chat, _, mut rx, _) = make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat;
    app.app_server_target = AppServerTarget::LocalDaemon {
        allow_embedded_fallback: true,
        endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
            socket_path: AbsolutePathBuf::relative_to_current_dir("codex.sock").unwrap(),
        },
    };
    app.chat_widget.remote_connection = Some(RemoteConnectionStatus {
        address: "local".into(),
        version: "v0.156.0".into(),
        is_local_daemon: true,
        is_managed_daemon: true,
    });
    app.open_daemon_menu();
    insta::assert_snapshot!(
        "daemon_menu",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
    assert!(rx.try_recv().is_err());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    assert!(rx.try_recv().is_err());

    app.app_server_target = AppServerTarget::Embedded;
    app.chat_widget.remote_connection = None;
    app.open_daemon_menu();
    insta::assert_snapshot!(
        "daemon_disconnected",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn remote_daemon_menu_reports_status_without_update_actions() {
    let mut app = make_test_app().await;
    let (chat, _, mut rx, _) = make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat;
    app.app_server_target = AppServerTarget::Remote {
        endpoint: crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url: "ws://example.test:1234".into(),
            auth_token: None,
        },
    };
    app.chat_widget.remote_connection = Some(RemoteConnectionStatus {
        address: "ws://example.test:1234".into(),
        version: "v0.153.0".into(),
        is_local_daemon: false,
        is_managed_daemon: false,
    });
    app.open_daemon_menu();
    insta::assert_snapshot!(
        "daemon_remote_status",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    assert!(rx.try_recv().is_err());
}
