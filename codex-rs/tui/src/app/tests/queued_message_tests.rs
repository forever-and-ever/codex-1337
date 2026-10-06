use super::*;
use crate::app::QueueEvent;
use crate::app_command::PendingSteerCancelAction;
use codex_app_server_protocol::QueuedSubmission;
use codex_app_server_protocol::TurnToolOutput;
use codex_protocol::models::FunctionCallOutputBody;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn queued_edit_acknowledgement_preserves_the_original_threads_draft() -> Result<()> {
    let (mut app, _home) = make_history_test_app().await?;
    let (server, _, proxy) = start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let original = ThreadId::new();
    let item = QueuedSubmission {
        id: "incoming".into(),
        client_user_message_id: "incoming".into(),
        input: Vec::new(),
        tool_output: Some(Box::new(TurnToolOutput {
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
            super::super::queued_messages::QueueMutation {
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
