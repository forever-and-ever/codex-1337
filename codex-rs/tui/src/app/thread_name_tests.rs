use super::*;
use crate::chatwidget::tests::helpers::render_bottom_popup;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadNameConflict;
use codex_utils_path_uri::LegacyAppPathString;
use crossterm::event::KeyCode;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn name_conflict_cancels_by_default_and_confirms_concrete_owners() {
    let (mut app, mut rx, _op_rx) = crate::app::tests::make_test_app_with_channels().await;
    let owner = ThreadId::from_u128(/*value*/ 1).to_string();
    let request = ThreadNameRequest::Rename {
        thread_id: ThreadId::from_u128(/*value*/ 2),
        name: "Repair login".to_string(),
    };
    let error = TypedRequestError::Server {
        method: "thread/name/set".to_string(),
        source: JSONRPCErrorError {
            code: -32602,
            message: "Name is already in use".to_string(),
            data: Some(
                serde_json::to_value(ThreadNameConflictError::Conflict {
                    name: "Repair login".to_string(),
                    conflicts: vec![ThreadNameConflict {
                        thread_id: owner.clone(),
                        name: "Repair login".to_string(),
                        cwd: LegacyAppPathString::from_string("/work/repair-login"),
                        status: ThreadStatus::Idle,
                    }],
                    archive_stops_descendants: true,
                })
                .unwrap(),
            ),
        },
    };
    assert!(app.confirm_thread_name_conflict(&error, request.clone()));
    insta::assert_snapshot!(
        "thread_name_conflict",
        render_bottom_popup(&app.chat_widget, /*width*/ 78)
    );
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    assert!(!app.chat_widget.has_active_view());
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::ConfirmedThreadName { .. }))
    );

    assert!(app.confirm_thread_name_conflict(&error, request.clone()));
    app.chat_widget.handle_key_event(KeyCode::Char('2').into());
    assert!(app.chat_widget.has_active_view());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    let confirmations = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::ConfirmedThreadName {
                request,
                conflict_thread_ids,
            } => Some((request, conflict_thread_ids)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(confirmations, vec![(request, vec![owner])]);
}
