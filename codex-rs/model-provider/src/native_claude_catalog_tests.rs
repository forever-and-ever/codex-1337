use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn paginated_native_catalog_preserves_capabilities_through_canonical_cache() {
    let haiku = json!({
        "id": "claude-haiku-4-5-20251001", "display_name": "Claude Haiku 4.5",
        "max_input_tokens": 200_000, "max_tokens": 64000,
        "capabilities": {
            "thinking": {"supported":true,"types":{"enabled":{"supported":true}}},
            "structured_outputs":{"supported":true}, "image_input":{"supported":true},
            // Threshold compaction must not enable the signed on-demand protocol.
            "context_management":{"compact_20260112":{"supported":true}}
        }
    });
    let sonnet = json!({
        "id":"claude-sonnet-5-5", "display_name":"Claude Sonnet 5.5",
        "max_input_tokens":1_000_000, "max_tokens":128_000,
        "capabilities": {
            "thinking":{"supported":true,"types":{"adaptive":{"supported":true}}},
            "effort":{"supported":true,"low":{"supported":true},"medium":{"supported":true},
                "high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":true}},
            "compaction":{"supported":true,"summarize":{"supported":true}},
            "structured_outputs":{"supported":true},"image_input":{"supported":true}
        }
    });
    // Authentication rejected while fetching a page must survive its API-error boundary.
    let required = codex_protocol::sign_in_required::SignInRequired {
        provider: codex_protocol::sign_in_required::AccountProvider::ClaudeSubscription,
        account: "claude@example.test".into(),
    };
    let error = list_models(|_| {
        std::future::ready(Err::<Page, _>(PageError::Auth(
            codex_protocol::error::CodexErrorDetails::SignInRequired(required.clone()).into(),
        )))
    })
    .await
    .err()
    .unwrap();
    assert!(
        matches!(error.into_core(codex_api::map_api_error).details(),
        codex_protocol::error::CodexErrorDetails::SignInRequired(actual) if actual == &required)
    );
    let mut cursors = Vec::new();
    let (models, etag) = list_models(|cursor| {
        cursors.push(cursor.clone());
        let page = match cursor.as_deref() {
            None => json!({"data":[haiku],"has_more":true,"last_id":"claude-haiku-4-5-20251001"}),
            Some("claude-haiku-4-5-20251001") => {
                json!({"data":[sonnet],"has_more":false,"last_id":"claude-sonnet-5-5"})
            }
            Some(_) => unreachable!(),
        };
        async move { Ok::<Page, ApiError>(serde_json::from_value(page).unwrap()) }
    })
    .await
    .unwrap();
    assert_eq!(
        cursors,
        vec![None, Some("claude-haiku-4-5-20251001".into())]
    );
    assert_eq!(etag, None);
    let restored: Vec<ModelInfo> =
        serde_json::from_slice(&serde_json::to_vec(&models).unwrap()).unwrap();
    assert_eq!(restored, models);
    let facts = models
        .iter()
        .map(|model| {
            let native = model.anthropic.as_ref().unwrap();
            (
                model.slug.as_str(),
                model.context_window,
                native.clone(),
                model.supports_native_compaction,
                model
                    .supported_reasoning_levels
                    .iter()
                    .map(|preset| preset.effort.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        facts,
        vec![
            (
                "claude-haiku-4-5-20251001",
                Some(200_000),
                AnthropicModelCapabilities {
                    thinking: AnthropicThinkingMode::Manual,
                    max_output_tokens: NonZeroU32::new(/*n*/ 64000).unwrap(),
                    supported_efforts: vec![],
                    supports_structured_outputs: true,
                },
                false,
                vec![
                    ReasoningEffort::None,
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                    ReasoningEffort::XHigh
                ]
            ),
            (
                "claude-sonnet-5-5",
                Some(1_000_000),
                AnthropicModelCapabilities {
                    thinking: AnthropicThinkingMode::Adaptive,
                    max_output_tokens: NonZeroU32::new(/*n*/ 128_000).unwrap(),
                    supported_efforts: vec![
                        ReasoningEffort::Low,
                        ReasoningEffort::Medium,
                        ReasoningEffort::High,
                        ReasoningEffort::XHigh,
                        ReasoningEffort::Max
                    ],
                    supports_structured_outputs: true,
                },
                true,
                vec![
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                    ReasoningEffort::XHigh,
                    ReasoningEffort::Max
                ]
            ),
        ]
    );
}
