use super::*;

#[tokio::test]
async fn completed_turn_does_not_erase_local_or_in_flight_input() {
    let (mut chat, _events, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    assert!(!chat.ready_for_idle_restart());
    handle_turn_started(&mut chat, "turn-1");
    assert!(!chat.ready_for_idle_restart());
    handle_turn_completed(&mut chat, "turn-1", /*duration_ms*/ None);
    assert!(chat.ready_for_idle_restart());

    chat.bottom_pane
        .set_composer_text("unsent draft".into(), Vec::new(), Vec::new());
    assert!(!chat.ready_for_idle_restart());
    let saved = chat.capture_thread_input_state().unwrap();
    assert!(saved.has_unsent_input());
    chat.bottom_pane
        .set_composer_text(String::new(), Vec::new(), Vec::new());
    assert!(chat.ready_for_idle_restart());
    assert!(
        !chat
            .capture_thread_input_state()
            .unwrap()
            .has_unsent_input()
    );
    // Clearing the foreground composer does not make a saved background draft disposable.
    assert!(saved.has_unsent_input());

    chat.bottom_pane
        .set_composer_text("next turn".into(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(chat.composer_is_empty());
    assert!(!chat.ready_for_idle_restart());
    handle_turn_started(&mut chat, "turn-2");
    handle_turn_completed(&mut chat, "turn-2", /*duration_ms*/ None);
    assert!(chat.ready_for_idle_restart());
    assert!(
        !chat
            .capture_thread_input_state()
            .unwrap()
            .has_unsent_input()
    );

    chat.bottom_pane.show_selection_view(SelectionViewParams {
        title: Some("Choose".into()),
        ..Default::default()
    });
    assert!(!chat.ready_for_idle_restart());
}
