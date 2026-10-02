use super::*;
use crate::BearerAuthProvider;
use codex_api::Provider;
use codex_api::RetryConfig;
use codex_http_client::HttpClientBuilder;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::headers;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::query_param;

#[tokio::test]
async fn paginated_native_catalog_preserves_capabilities_through_canonical_cache() {
    let server = MockServer::start().await;
    let haiku = json!({
        "id": "claude-haiku-4-5-20251001", "display_name": "Claude Haiku 4.5",
        "max_input_tokens": 200000, "max_tokens": 64000,
        "capabilities": {
            "thinking": {"supported":true,"types":{"enabled":{"supported":true}}},
            "structured_outputs":{"supported":true}, "image_input":{"supported":true},
            // Threshold compaction must not enable the signed on-demand protocol.
            "context_management":{"compact_20260112":{"supported":true}}
        }
    });
    let sonnet = json!({
        "id":"claude-sonnet-5-5", "display_name":"Claude Sonnet 5.5",
        "max_input_tokens":1000000, "max_tokens":128000,
        "capabilities": {
            "thinking":{"supported":true,"types":{"adaptive":{"supported":true}}},
            "effort":{"supported":true,"low":{"supported":true},"medium":{"supported":true},
                "high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":true}},
            "compaction":{"supported":true,"summarize":{"supported":true}},
            "structured_outputs":{"supported":true},"image_input":{"supported":true}
        }
    });
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param("limit", "100"))
        .and(header("authorization", "Bearer test-catalog-token"))
        .and(headers(
            "anthropic-beta",
            vec!["oauth-2025-04-20", "compact-2026-09-04"],
        ))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "data":[haiku],"has_more":true,"last_id":"claude-haiku-4-5-20251001"
        })))
        .up_to_n_times(/*n*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param("after_id", "claude-haiku-4-5-20251001"))
        .and(header("authorization", "Bearer test-catalog-token"))
        .and(headers(
            "anthropic-beta",
            vec!["oauth-2025-04-20", "compact-2026-09-04"],
        ))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "data":[sonnet],"has_more":false,"last_id":"claude-sonnet-5-5"
        })))
        .mount(&server)
        .await;
    let transport =
        ReqwestTransport::from_http_client(HttpClientBuilder::new().build_direct().unwrap());
    let client = ModelsClient::new(
        transport,
        Provider {
            name: "anthropic-test".into(),
            base_url: format!("{}/v1", server.uri()),
            query_params: None,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(/*millis*/ 1),
                retry_429: false,
                retry_5xx: false,
                retry_transport: false,
            },
            stream_idle_timeout: Duration::from_secs(/*secs*/ 1),
        },
        Arc::new(BearerAuthProvider::new("test-catalog-token".into())),
    );
    let (models, etag) = list_models(&client, format!("{}/v1/models", server.uri()))
        .await
        .unwrap();
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
                Some(200000),
                AnthropicModelCapabilities {
                    thinking: AnthropicThinkingMode::Manual,
                    max_output_tokens: NonZeroU32::new(/*n*/ 64000).unwrap(),
                    supported_efforts: vec![],
                    supports_mid_conversation_system: false,
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
                Some(1000000),
                AnthropicModelCapabilities {
                    thinking: AnthropicThinkingMode::Adaptive,
                    max_output_tokens: NonZeroU32::new(/*n*/ 128000).unwrap(),
                    supported_efforts: vec![
                        ReasoningEffort::Low,
                        ReasoningEffort::Medium,
                        ReasoningEffort::High,
                        ReasoningEffort::XHigh,
                        ReasoningEffort::Max
                    ],
                    supports_mid_conversation_system: true,
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
