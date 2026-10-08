use super::*;
use codex_app_server_protocol::ContextRole;

#[tokio::test]
async fn context_role_command_output() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    let mut role = None;
    let mut output = Vec::new();
    for argument in ["", "  Review agent boundaries  ", "", "clear", "bad\nrole"] {
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
}
