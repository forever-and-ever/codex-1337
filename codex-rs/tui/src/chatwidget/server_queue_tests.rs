use super::*;
use crate::chatwidget::KeyCode;
use crate::chatwidget::KeyModifiers;
use crate::chatwidget::ThreadId;
use crate::chatwidget::UserMessage;
use crate::chatwidget::tests::helpers::render_bottom_popup;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use codex_app_server_protocol::TurnToolOutput;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn incoming_messages_share_queue_controls_without_local_autosend() {
    let (mut chat, _, mut events, mut ops) = Box::pin(make_chatwidget_manual_with_sender()).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.apply_external_edit("unfinished prompt".into());
    let item = QueuedSubmission {
        responsesapi_client_metadata: None,
        id: "queued-1".into(), client_user_message_id: "queued-1".into(), input: Vec::new(),
        tool_output: Some(Box::new(TurnToolOutput {
                retain: false,
            name: "send_message_to_thread".into(), namespace: Some("codex_tui".into()),
            output: FunctionCallOutputBody::Text("<codex_delegation>\n  <source_thread_id>source</source_thread_id>\n  <source_thread_name>SYSADMIN</source_thread_name>\n  <input>Check the service after your current turn.</input>\n</codex_delegation>".into()),
        })),
    };
    chat.update_server_queue(vec![item.clone()], /*busy*/ false);
    chat.maybe_send_next_queued_input();
    assert!(
        ops.try_recv().is_err(),
        "the durable queue, not this TUI, owns dispatch"
    );
    insta::assert_snapshot!(
        "incoming_queued_message",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    // Removal must be acknowledged before the incoming message becomes a draft.
    assert_eq!(chat.composer_text_with_pending(), "unfinished prompt");
    // The widget emits intents; App's mutation owner admits only one operation.
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
    let actions = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::Queue(QueueEvent::Remove {
                thread_id,
                item,
                action,
            }) => Some((thread_id, item.id, action)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        vec![
            (thread_id, item.id.clone(), PendingSteerCancelAction::Edit),
            (thread_id, item.id.clone(), PendingSteerCancelAction::Remove),
        ]
    );
    chat.finish_server_queue_removal(&item.id, &Ok(true));
    chat.restore_user_message_to_composer(UserMessage::from(queued_message_text(&item).unwrap()));
    assert_eq!(
        chat.composer_text_with_pending(),
        "Check the service after your current turn.\nunfinished prompt"
    );
    assert_eq!(chat.input_queue_frontend.server_messages.len(), 0);
}
