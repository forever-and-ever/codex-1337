use super::*;
use crate::create_model_provider;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::ModelsManagerConfig;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::openai_models::ModelsResponse;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn account_routes_do_not_leak_into_catalog_or_admit_openai_fallbacks() {
    let speak_claude = |model: &mut ModelInfo| {
        if !model
            .experimental_supported_tools
            .iter()
            .any(|tool| tool == REQUEST_USER_INPUT_ASYNC)
        {
            model
                .experimental_supported_tools
                .push(REQUEST_USER_INPUT_ASYNC.to_string());
        }
        model
            .model_messages
            .get_or_insert_default()
            .multi_agent
            .get_or_insert_default()
            .role = Some(MultiAgentRoleMessages {
            root: Some(ROOT_AGENT_ROLE.to_string()),
            subagent: Some(SUBAGENT_ROLE.to_string()),
        });
    };
    let unrelated = codex_models_manager::bundled_models_response()
        .unwrap()
        .models
        .remove(0);
    let mut first = unrelated.clone();
    first.slug = "claude-sonnet-5-5".into();
    first.display_name = first.slug.clone();
    first.context_window = Some(1_000_000);
    first.max_context_window = Some(1_000_000);
    let source_catalog = ModelsResponse {
        models: vec![first.clone(), unrelated],
    };
    let provider = create_model_provider(
        ModelProviderInfo {
            adapter: ModelProviderAdapter::ClaudeSubscription,
            ..Default::default()
        },
        /*auth_manager*/ None,
    );
    let manager = provider.models_manager_without_cache(/*config_model_catalog*/ None);
    manager.set_api_key_model_discovery_enabled(true);
    assert_eq!(manager.try_list_models().unwrap(), []);
    // A resumed model must not change tool protocol while catalog discovery is unavailable.
    let missing_slug = "claude-haiku-4-5-20251001";
    let mut missing = codex_models_manager::model_info::model_info_from_slug(missing_slug);
    missing.tool_mode = Some(ToolMode::CodeModeOnly);
    missing.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    missing.multi_agent_version = Some(MultiAgentVersion::V2);
    missing.context_window = Some(200_000);
    missing.max_context_window = Some(200_000);
    speak_claude(&mut missing);
    let context_override = ModelsManagerConfig {
        model_context_window: Some(1_000_000),
        ..Default::default()
    };
    assert_eq!(
        manager
            .get_model_info(missing_slug, &context_override)
            .await
            .model
            .as_ref(),
        &missing
    );
    first.slug = "claude-sonnet-5-5".into();
    first.display_name = first.slug.clone();
    first.tool_mode = Some(ToolMode::CodeModeOnly);
    first.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    first.multi_agent_version = Some(MultiAgentVersion::V2);
    first.additional_speed_tiers.clear();
    first.service_tiers.clear();
    first.default_service_tier = None;
    first.supported_in_api = true;
    speak_claude(&mut first);
    let expected = ModelsResponse {
        models: vec![first.clone()],
    };
    let configured = provider.models_manager_without_cache(Some(source_catalog));
    assert_eq!(
        configured
            .raw_model_catalog(
                RefreshStrategy::Offline,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await,
        expected
    );
    assert_eq!(
        configured
            .get_model_info(missing_slug, &context_override)
            .await
            .model
            .as_ref(),
        &missing
    );
    assert_eq!(
        configured
            .get_model_info(&first.slug, &context_override)
            .await
            .model
            .as_ref(),
        &first
    );
}
