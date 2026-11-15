//! Model-family presentation at constrained sizes and with configured list bindings.

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn model_picker_compact_hint_and_actions_follow_configured_bindings() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.list.accept = vec![key_hint::plain(KeyCode::F(3))];
    keymap.list.cancel = vec![key_hint::plain(KeyCode::F(2))];
    chat.bottom_pane.set_keymap_bindings(&keymap);
    chat.open_all_models_popup();
    let popup = render_bottom_popup(&chat, /*width*/ 40);
    assert_eq!(popup.lines().last().unwrap().trim(), "f3 select · f2 back");
    chat.handle_key_event(KeyEvent::from(KeyCode::F(3)));
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenReasoningPopup { .. }));
    chat.handle_key_event(KeyEvent::from(KeyCode::F(2)));
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn model_picker_opens_after_discovery_populates_an_empty_catalog() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    let crate::app_server_session::BootstrapAccount::Loaded(account) = std::mem::replace(
        &mut chat.account,
        crate::app_server_session::BootstrapAccount::AwaitingResume,
    ) else {
        panic!("manual fixture has a captured payer");
    };
    chat.update_backend_authority(crate::app_server_session::BackendAuthority::Reported(Some(
        codex_app_server_protocol::AuthMode::Headers,
    )));
    chat.open_model_popup();
    assert!(chat.model_popup_request.is_none());
    assert!(chat.status_account_display().is_none());
    assert!(chat.current_plan_type().is_none());
    assert!(chat.has_codex_backend_auth());
    let refusal = drain_insert_history_normalized(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(refusal.contains(chat.account_ready_reason().expect("pending payer reason")));
    chat.update_account_state(account);
    assert!(chat.account_ready_reason().is_none());
    assert!(chat.has_codex_backend_auth());
    chat.thread_id = Some(ThreadId::new());
    let presets = std::mem::take(&mut Arc::make_mut(&mut chat.model_catalog).models);
    chat.open_model_popup();
    let loading = render_bottom_popup(&chat, /*width*/ 80);
    let loading_title = loading.lines().find(|line| line.contains("Loading models"));
    insta::assert_snapshot!(loading_title.unwrap().trim(), @"Loading models…");
    let obsolete = chat.model_popup_request.as_ref().unwrap().id();
    chat.handle_key_event(KeyCode::Esc.into());
    assert!(chat.no_modal_or_popup_active());
    chat.open_model_popup();
    assert!(!chat.on_models_loaded(
        obsolete,
        Ok(crate::model_catalog::ModelListing {
            models: presets.clone(),
            providers: chat.model_catalog.providers.clone()
        })
    ));
    let request_id = chat.model_popup_request.as_ref().unwrap().id();
    assert!(chat.on_models_loaded(
        request_id,
        Ok(crate::model_catalog::ModelListing {
            models: presets,
            providers: chat.model_catalog.providers.clone()
        })
    ));
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_selection_after_discovery", popup);
}
