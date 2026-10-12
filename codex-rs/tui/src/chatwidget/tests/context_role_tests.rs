use super::*;
use codex_app_server_protocol::ContextRole;

#[tokio::test]
async fn context_role_command_output() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    let mut role = None;
    let mut output = Vec::new();
    for argument in [
        "",
        "set Review agent boundaries",
        "",
        "clear",
        "set bad\nrole",
    ] {
        chat.dispatch_command_with_args(SlashCommand::ContextRole, argument.to_owned(), Vec::new());
        while let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::ContextRole { command, .. } => match command {
                    ContextRoleCommand::Show => {
                        chat.show_context_role(role.as_ref().map(ContextRole::as_str));
                    }
                    ContextRoleCommand::Set(next) => {
                        role = next;
                        if let Some(role) = &role {
                            chat.show_context_role(Some(role.as_str()));
                        } else {
                            chat.context_role_cleared();
                        }
                    }
                },
                AppEvent::InsertHistoryCell(cell) => {
                    output.push(lines_to_single_string(&cell.display_lines(/*width*/ 80)));
                }
                _ => {}
            }
        }
    }
    insta::assert_snapshot!(output.join("\n"));

    for input in [
        "/contextrole claer",
        "/contextrole Review agent boundaries",
        "/contextrole set",
        "/contextrole clear extra",
    ] {
        chat.bottom_pane
            .set_composer_text(input.to_string(), Vec::new(), Vec::new());
        chat.handle_key_event(KeyCode::Esc.into());
        chat.handle_key_event(KeyCode::Enter.into());
        let mut errors = 0;
        while let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::FollowTranscript => {}
                AppEvent::InsertHistoryCell(cell) => {
                    assert!(
                        lines_to_single_string(&cell.display_lines(200))
                            .contains("Usage: /contextrole")
                    );
                    errors += 1;
                }
                event => panic!("invalid role command emitted {event:?}"),
            }
        }
        assert_eq!(errors, 1, "{input}");
        assert_eq!(chat.bottom_pane.composer_text(), input);
        assert!(role.is_none());
    }
}
