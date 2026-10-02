//! Resolves the classifier's own model budget and transport configuration.
//! Parent-model context overrides do not change the classifier allowance.

use std::sync::Arc;

use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_extension_api::ThreadOriginator;
use codex_extension_api::ThreadStartInput;
use codex_features::Feature;
use codex_login::AgentIdentityAuthPolicy;
use codex_login::AuthManager;
use codex_model_provider::create_model_provider;

use super::sampler::LunaSamplerConfig;
use super::sampler::MODEL;

pub(super) async fn sampler_config(
    input: &ThreadStartInput<'_, Config>,
    auth_manager: Arc<AuthManager>,
    manager: Option<Arc<ThreadManager>>,
) -> LunaSamplerConfig {
    let provider = create_model_provider(
        input.config.model_provider.clone(),
        Some(
            input
                .thread_store
                .get::<Arc<AuthManager>>()
                .as_deref()
                .cloned()
                .unwrap_or(auth_manager),
        ),
    );
    let native = !matches!(provider.anthropic_auth_context(), Ok(None));
    // Native discovery waits for the classification's committed conversation identity.
    let luna_model = if native {
        None
    } else if let Some(thread_manager) = manager {
        let mut model_config = input.config.to_models_manager_config();
        model_config.model_context_window = None;
        Some(
            input
                .thread_store
                .get::<codex_models_manager::manager::SharedModelsManager>()
                .as_deref()
                .cloned()
                .unwrap_or_else(|| thread_manager.get_models_manager())
                .get_model_info(MODEL, &model_config)
                .await,
        )
    } else {
        None
    };
    let max_input_tokens = luna_model.as_ref().map_or(
        if native {
            0
        } else {
            codex_guardian_context::DEFAULT_MAX_INPUT_TOKENS
        },
        |model| {
            codex_guardian_context::effective_input_token_limit(
                model, /*configured_window*/ None,
            )
        },
    );
    let luna_compaction_hash = luna_model.and_then(|model| model.comp_hash);
    LunaSamplerConfig {
        workspace_routing: input
            .thread_store
            .get_or_init(|| input.config.workspace_routing_context())
            .as_ref()
            .clone(),
        provider,
        http_client_factory: input.config.http_client_factory(),
        agent_identity_policy: if input.config.features.enabled(Feature::UseAgentIdentity) {
            AgentIdentityAuthPolicy::ChatGptAuth
        } else {
            AgentIdentityAuthPolicy::JwtOnly
        },
        session_source: input.session_source.clone(),
        session_id: input.session_store.level_id().to_string(),
        thread_id: input.thread_store.level_id().to_string(),
        originator: input
            .thread_store
            .get::<ThreadOriginator>()
            .map(|originator| originator.0.clone()),
        service_tier: input.config.service_tier.clone(),
        luna_compaction_hash,
        max_input_tokens,
        metrics: input.extension_metrics.clone(),
    }
}
