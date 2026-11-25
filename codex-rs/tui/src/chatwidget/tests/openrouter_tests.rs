use super::*;
use crate::openrouter::Event;
use crate::openrouter::ModelMenu;
use crate::openrouter::StatusRefresh;
use codex_app_server_protocol::OpenRouterKey;
use codex_app_server_protocol::OpenRouterKeyLimit;
use codex_app_server_protocol::OpenRouterKeyState;
use codex_app_server_protocol::OpenRouterKeyStatus;
use codex_app_server_protocol::OpenRouterSpend;
use codex_protocol::Usd;

#[tokio::test]
async fn openrouter_model_menu_snapshot() {
    use crate::app::ProviderModelEvent;
    use crate::openrouter::Request;
    use codex_app_server_protocol::ModelProviderOption;

    let (mut chat, mut rx, _op_rx) = Box::pin(make_chatwidget_manual(None)).await;
    let mut model = chat.model_catalog.models[0].clone();
    model.model = "moonshotai/kimi-k3".into();
    model.display_name = "Kimi K3".into();
    model.description = "$0.30 in · $11.94 out per M · 1M context".into();
    let mut preset = model.clone();
    preset.model = "@preset/research".into();
    preset.display_name = "research".into();
    preset.description = "preset for moonshotai/kimi-k3".into();
    let models = vec![model, preset];
    let providers = [
        ("openai", "OpenAI"),
        ("claude", "Claude"),
        (
            codex_model_provider_info::OPENROUTER_PROVIDER_ID,
            "OpenRouter",
        ),
    ]
    .map(|(id, name)| ModelProviderOption {
        id: id.into(),
        name: name.into(),
    })
    .to_vec();
    let selected = "moonshotai/kimi-k3".parse().unwrap();
    chat.show_openrouter_models(
        ModelMenu::New(providers[2].clone()),
        crate::model_catalog::ModelListing {
            models: models.clone(),
            providers: providers.clone(),
        },
        Some(&selected),
        None,
    );
    assert_chatwidget_snapshot!("openrouter_model_menu", render_bottom_popup(&chat, 100));
    chat.handle_key_event(KeyEvent::from(KeyCode::Esc));
    chat.config.model_provider_id = codex_model_provider_info::OPENROUTER_PROVIDER_ID.into();
    chat.set_model("moonshotai/kimi-k3");

    for visible_models in [models, Vec::new()] {
        // Provider rows must stay outside the model-only endpoint/forget hotkey prefix,
        // including an empty OpenRouter catalog; entering one uses the new-thread flow.
        for (offset, provider) in providers[..2].iter().enumerate() {
            chat.show_openrouter_models(
                ModelMenu::Current,
                crate::model_catalog::ModelListing {
                    models: visible_models.clone(),
                    providers: providers.clone(),
                },
                Some(&selected),
                None,
            );
            let rendered = render_bottom_popup(&chat, 100);
            assert!(rendered.contains("OpenAI…"));
            assert!(rendered.contains("Claude…"));
            while rx.try_recv().is_ok() {}

            if !visible_models.is_empty() {
                chat.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
                assert!(matches!(
                    rx.try_recv(),
                    Ok(AppEvent::OpenRouter(Event::Request(Request::Endpoints { slug, .. })))
                        if slug == selected
                ));
                chat.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
                assert!(matches!(
                    rx.try_recv(),
                    Ok(AppEvent::OpenRouter(Event::Request(Request::Forget { slug, .. })))
                        if slug == selected
                ));
            }
            for _ in 0..visible_models.len() + 1 + offset {
                chat.handle_key_event(KeyEvent::from(KeyCode::Down));
            }
            chat.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
            chat.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
            assert!(rx.try_recv().is_err());
            chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
            assert!(matches!(
                rx.try_recv(),
                Ok(AppEvent::ProviderModel(ProviderModelEvent::Open(choice)))
                    if choice.id == provider.id
            ));
            assert_eq!(chat.current_model(), selected.as_str());
            assert_eq!(
                chat.config.model_provider_id,
                codex_model_provider_info::OPENROUTER_PROVIDER_ID
            );
        }
    }
}

#[tokio::test]
async fn openrouter_status_snapshot() {
    let (mut chat, mut rx, _op_rx) = Box::pin(make_chatwidget_manual(None)).await;
    chat.config.model_provider_id = codex_model_provider_info::OPENROUTER_PROVIDER_ID.into();
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.token_info = Some(make_token_info(1000, 128_000));
    chat.token_info.as_mut().unwrap().total_token_usage.cost =
        Some(Usd::from_nanodollars(420_000_000));
    chat.refresh_openrouter_status(StatusRefresh::Show);
    let mut request = None;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::OpenRouter(Event::RefreshStatus { id, .. }) = event {
            request = Some(id);
        }
    }
    chat.finish_openrouter_status(
        thread_id,
        request.unwrap(),
        StatusRefresh::Show,
        &Ok(OpenRouterKeyStatus {
            outstanding_batches: 0,
            key: OpenRouterKey {
                name: "main".parse().unwrap(),
                path: AbsolutePathBuf::try_from("/secrets/openrouter.token").unwrap(),
                is_default: true,
            },
            state: OpenRouterKeyState::Read {
                label: "sk-or-v1-…abcd".into(),
                balance: Some(Usd::from_nanodollars(24_620_000_000)),
                limit: Some(OpenRouterKeyLimit {
                    limit: Usd::from_nanodollars(30_000_000_000),
                    remaining: Usd::from_nanodollars(20_000_000_000),
                    reset: Some("monthly".into()),
                }),
                spend: OpenRouterSpend {
                    daily: Usd::from_nanodollars(420_000_000),
                    weekly: Usd::from_nanodollars(1_760_000_000),
                    monthly: Usd::from_nanodollars(3_880_000_000),
                },
                expires_at: None,
                captured_at: 0,
            },
        }),
    );
    assert_eq!(
        chat.status_line_value_for_item(StatusLineItem::WeeklyLimit)
            .as_deref(),
        Some("$20.00 left")
    );
    assert_eq!(
        chat.status_line_value_for_item(StatusLineItem::FiveHourLimit)
            .as_deref(),
        Some("$0.42 today")
    );
    let history = drain_insert_history(&mut rx);
    let block = history
        .last()
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert_chatwidget_snapshot!("openrouter_status", block);
}
