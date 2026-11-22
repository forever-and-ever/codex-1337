//! Builds reviewer instructions from the resolved model and trusted policy template.

use codex_features::Feature;
use codex_guardian_reviewer::reviewer_instructions;
use codex_prompts::ResolvedModelMessages;
use codex_protocol::models::BaseInstructionsProvenance;

use crate::config::Config;
use crate::config::NetworkProxySpec;
use crate::context::ContextualUserFragment;
use crate::context::GuardianConversationHistory;
use crate::guardian_review::ReviewRetrieval;

pub use super::reviewer_catalog::guardian_policy_model;
pub(crate) use super::reviewer_catalog::resolve_review_model;

/// Adds the captured model, policy prompt and live network rules before reuse selection.
#[allow(clippy::too_many_arguments)]
pub fn build_guardian_review_session_config(
    mut guardian_config: Config,
    live_network_config: Option<codex_network_proxy::NetworkProxyConfig>,
    active_model: &str,
    reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    reasoning_summary: codex_protocol::config_types::ReasoningSummary,
    personality: Option<codex_protocol::config_types::Personality>,
    model_messages: ResolvedModelMessages<'_>,
    retrieval: ReviewRetrieval,
) -> anyhow::Result<Config> {
    guardian_config.model = Some(active_model.to_owned());
    guardian_config.model_reasoning_effort = reasoning_effort;
    guardian_config.model_reasoning_summary = Some(reasoning_summary);
    guardian_config.personality = personality;
    // The model catalog can come from user configuration. Managed rules need a trusted
    // template with policy slots unless the host explicitly supplies its own template.
    let requirements = guardian_config.config_layer_stack.requirements_toml();
    let needs_trusted_template = requirements
        .guardian_policy_config
        .iter()
        .chain(&guardian_config.guardian_extra_policy)
        .any(|policy| !policy.trim().is_empty());
    let policy_template = match guardian_config.guardian_policy_template.as_deref() {
        None if needs_trusted_template => Some(
            ResolvedModelMessages::bundled()
                .auto_review()
                .policy_template,
        ),
        template => template,
    };
    let retrieval_guidance = match retrieval {
        ReviewRetrieval::ConversationHistory
            if guardian_config
                .features
                .enabled(Feature::GuardianConversationHistoryTools)
                && guardian_config.features.enabled(Feature::Apps) =>
        {
            GuardianConversationHistory {
                prompt: guardian_config
                    .guardian_conversation_history_prompt
                    .as_deref(),
            }
            .render()
            .into_text()
        }
        ReviewRetrieval::ConversationHistory => None,
        ReviewRetrieval::Extension(prompt) => Some(prompt.to_owned()),
    };
    let instructions = reviewer_instructions(
        guardian_config.guardian_transcript_mode,
        guardian_config.resolve_guardian_policy(model_messages),
        guardian_config.guardian_extra_policy.as_deref(),
        policy_template,
        model_messages,
        retrieval_guidance.as_deref(),
    );
    guardian_config.base_instructions = Some(instructions);
    guardian_config.base_instructions_provenance = Some(BaseInstructionsProvenance::Custom);
    if let Some(live_network_config) = live_network_config
        && guardian_config.permissions.network.is_some()
    {
        let network_constraints = guardian_config
            .config_layer_stack
            .requirements()
            .network
            .as_ref()
            .map(|network| network.value.clone());
        guardian_config.permissions.network = Some(NetworkProxySpec::from_config_and_constraints(
            live_network_config,
            network_constraints,
            guardian_config.permissions.permission_profile(),
        )?);
    }
    Ok(guardian_config)
}
