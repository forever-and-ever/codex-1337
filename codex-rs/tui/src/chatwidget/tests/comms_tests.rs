use super::*;
use codex_app_server_protocol::CommsGate;

#[tokio::test]
async fn comms_command_output() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    for comms in [CommsGate::Open, CommsGate::Closed, CommsGate::Blocked] {
        chat.show_comms(comms);
    }
    chat.dispatch_command_with_args(SlashCommand::Comms, "invalid".to_string(), Vec::new());
    let output = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(output);
}
