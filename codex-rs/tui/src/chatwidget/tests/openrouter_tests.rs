use super::*;
use crate::openrouter::Event;
use crate::openrouter::ModelMenu;
use crate::openrouter::StatusRefresh;
use codex_app_server_protocol::OpenRouterEndpoint;
use codex_app_server_protocol::OpenRouterHost;
use codex_app_server_protocol::OpenRouterKey;
use codex_app_server_protocol::OpenRouterKeyLimit;
use codex_app_server_protocol::OpenRouterKeyState;
use codex_app_server_protocol::OpenRouterKeyStatus;
use codex_app_server_protocol::OpenRouterModelEndpointsResponse;
use codex_app_server_protocol::OpenRouterSpend;
use codex_protocol::EndpointFilter;
use codex_protocol::EndpointSet;
use codex_protocol::Usd;
use std::collections::BTreeSet;

#[tokio::test]
async fn openrouter_model_menu_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    let mut model = chat.model_catalog.models[0].clone();
    model.model = "moonshotai/kimi-k3".into();
    model.display_name = "Kimi K3".into();
    model.description = "$0.30 in · $11.94 out per M · 1M context".into();
    let mut preset = model.clone();
    preset.model = "@preset/research".into();
    preset.display_name = "research".into();
    preset.description = "preset for moonshotai/kimi-k3".into();
    chat.show_openrouter_models(
        ModelMenu::New(codex_app_server_protocol::ModelProviderOption {
            id: codex_model_provider_info::OPENROUTER_PROVIDER_ID.into(),
            name: "OpenRouter".into(),
        }),
        crate::model_catalog::ModelListing {
            models: vec![model, preset],
            providers: Vec::new(),
        },
        Some("moonshotai/kimi-k3".parse().unwrap()),
        None,
    );
    assert_chatwidget_snapshot!("openrouter_model_menu", render_bottom_popup(&chat, 100));
}

#[tokio::test]
async fn openrouter_endpoints_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    let host = |slug: &str,
                name: &str,
                quantization: &str,
                tier: &str,
                input: i64,
                output: i64,
                uptime| OpenRouterHost {
        slug: slug.parse().unwrap(),
        name: name.into(),
        endpoints: vec![OpenRouterEndpoint {
            slug: format!("{slug}/turbo").parse().unwrap(),
            quantization: Some(quantization.into()),
            service_tier: Some(tier.into()),
            context_length: 1_000_000,
            prompt_per_mtok: Some(Usd::from_nanodollars(input)),
            completion_per_mtok: Some(Usd::from_nanodollars(output)),
            uptime_percent: Some(uptime),
        }],
    };
    let filter = EndpointFilter::Only(
        EndpointSet::new(BTreeSet::from([
            "deepinfra/turbo".parse().unwrap(),
            "old-host".parse().unwrap(),
        ]))
        .unwrap(),
    );
    chat.show_openrouter_endpoints(
        ModelMenu::Current,
        "moonshotai/kimi-k3".parse().unwrap(),
        OpenRouterModelEndpointsResponse {
            hosts: vec![
                host(
                    "deepseek",
                    "DeepSeek",
                    "fp8",
                    "priority",
                    300_000_000,
                    11_940_000_000,
                    99.9,
                ),
                host(
                    "deepinfra",
                    "DeepInfra",
                    "bf16",
                    "flex",
                    400_000_000,
                    12_000_000_000,
                    99.5,
                ),
            ],
            filter,
        },
    );
    assert_chatwidget_snapshot!("openrouter_endpoints", render_bottom_popup(&chat, 120));
}

#[tokio::test]
async fn openrouter_status_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.config.model_provider_id = codex_model_provider_info::OPENROUTER_PROVIDER_ID.into();
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.token_info = Some(make_token_info(1000, 128000));
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
        Ok(OpenRouterKeyStatus {
            key: OpenRouterKey {
                name: "main".parse().unwrap(),
                path: AbsolutePathBuf::try_from("/home/main/security/tokens/openrouter.token")
                    .unwrap(),
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
        chat.status_line_value(crate::bottom_pane::StatusLineItem::WeeklyLimit)
            .as_deref(),
        Some("$20.00 left")
    );
    assert_eq!(
        chat.status_line_value(crate::bottom_pane::StatusLineItem::FiveHourLimit)
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
