//! Prepares child configuration from captured step settings and requested overrides.
//!
//! Spawn and reload share live runtime policy; role and model precedence, full-history
//! inheritance, and validation messages remain the same for each multi-agent version.

use crate::agent::control::normalize_agent_service_tier;
use crate::agent::model_catalog::ProviderModels;
use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::role::apply_role_to_config;
use crate::agent::types::SpawnAgentForkMode;
use crate::config::Config;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::step_settings::ResolvedStepSettings;
use crate::session::turn_context::TurnContext;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::MultiAgentVersion;

pub(crate) const MAX_SPAWN_AGENT_MODEL_OVERRIDES: usize = 5;

pub(crate) fn model_supports_multi_agent_backend(
    model: &ModelPreset,
    multi_agent_version: MultiAgentVersion,
) -> bool {
    multi_agent_version != MultiAgentVersion::V2
        || model.multi_agent_version != Some(MultiAgentVersion::Disabled)
}

/// Selects the existing spawn tool's role-inheritance rules.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnConfigVersion {
    V1,
    V2,
}

/// Ordinary tool spawns retain role precedence; direct user executors own model selection.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnModelPrecedence {
    RoleOverrides,
    RequestedOverrides,
}

pub(crate) struct SpawnConfigOptions<'a> {
    pub(crate) version: SpawnConfigVersion,
    pub(crate) model_precedence: SpawnModelPrecedence,
    pub(crate) fork_mode: Option<&'a SpawnAgentForkMode>,
    pub(crate) role_name: Option<&'a str>,
    pub(crate) model: Option<&'a str>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) service_tier: Option<&'a str>,
}

pub(crate) struct PreparedSpawnConfig {
    pub(crate) config: Config,
    pub(crate) role_name: Option<String>,
}

/// Resolves child settings before starting the thread, retaining the invoking tool's precedence.
pub(crate) async fn prepare_agent_spawn_config(
    session: &Session,
    step_context: &StepContext,
    options: SpawnConfigOptions<'_>,
) -> Result<PreparedSpawnConfig, String> {
    prepare_agent_spawn_config_from_settings(
        session,
        step_context.turn.as_ref(),
        &step_context.settings,
        options,
    )
    .await
}

/// Also serves standalone user actions, which capture settings without preparing inference.
pub(crate) async fn prepare_agent_spawn_config_from_settings(
    session: &Session,
    turn: &TurnContext,
    settings: &ResolvedStepSettings,
    options: SpawnConfigOptions<'_>,
) -> Result<PreparedSpawnConfig, String> {
    let full_history_fork = matches!(options.fork_mode, Some(SpawnAgentForkMode::FullHistory));
    let mut config =
        build_agent_spawn_config(&session.get_base_instructions().await, turn, settings)?;
    if options.version == SpawnConfigVersion::V1 && full_history_fork {
        reject_full_fork_agent_type_override(options.role_name)?;
    }
    if options.model_precedence == SpawnModelPrecedence::RequestedOverrides {
        apply_spawn_role_inheritance(session, turn, &mut config, &options).await?;
        // Farm's absent executor preferences inherit the captured parent selection,
        // not a role's model. Explicit/general defaults still use the same resolver below.
        config.model = Some(settings.model_info.slug.clone());
        config.model_reasoning_effort = settings.effective_reasoning_effort();
    }
    apply_requested_spawn_agent_model_overrides(
        session,
        turn,
        settings,
        &mut config,
        options.model,
        options.reasoning_effort.clone(),
        options.fork_mode,
    )
    .await?;
    if options.model_precedence == SpawnModelPrecedence::RoleOverrides {
        apply_spawn_role_inheritance(session, turn, &mut config, &options).await?;
    }
    apply_spawn_agent_service_tier(session, &mut config, options.service_tier).await?;
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    // Remember an applied configured default so cold reload reapplies its restrictions.
    let role_name = options
        .role_name
        .or_else(|| {
            (options.version == SpawnConfigVersion::V2
                && !full_history_fork
                && config
                    .agent_roles
                    .get(DEFAULT_ROLE_NAME)
                    .is_some_and(|role| role.config_file.is_some()))
            .then_some(DEFAULT_ROLE_NAME)
        })
        .map(str::to_owned);
    Ok(PreparedSpawnConfig { config, role_name })
}

async fn apply_spawn_role_inheritance(
    session: &Session,
    turn: &TurnContext,
    config: &mut Config,
    options: &SpawnConfigOptions<'_>,
) -> Result<(), String> {
    let full_history_fork = matches!(options.fork_mode, Some(SpawnAgentForkMode::FullHistory));
    if !full_history_fork
        || (options.version == SpawnConfigVersion::V2 && options.role_name.is_some())
    {
        match options.model_precedence {
            SpawnModelPrecedence::RoleOverrides => {
                apply_spawn_agent_role(session, config, options.role_name).await?
            }
            SpawnModelPrecedence::RequestedOverrides => {
                apply_role_to_config(config, options.role_name).await?
            }
        }
        if options.version == SpawnConfigVersion::V2
            && full_history_fork
            && config.developer_instructions.is_none()
        {
            config
                .developer_instructions
                .clone_from(&turn.developer_instructions);
        }
    }
    Ok(())
}

/// Builds the base config snapshot for a newly spawned sub-agent.
///
/// The returned config starts from the parent's effective config and then refreshes the
/// model selection and reasoning settings captured for the invoking step, plus the turn's
/// runtime approval policy, sandbox, and cwd. Role-specific overrides are layered
/// after this step; skipping this helper and cloning stale config state directly can send the child
/// agent out with the wrong provider or runtime policy.
pub(crate) fn build_agent_spawn_config(
    base_instructions: &BaseInstructions,
    turn: &TurnContext,
    settings: &ResolvedStepSettings,
) -> Result<Config, String> {
    let mut config = build_agent_shared_config(turn)?;
    config.model = Some(settings.model_info.slug.clone());
    config.model_reasoning_effort = settings.effective_reasoning_effort();
    config.model_reasoning_summary = Some(settings.reasoning_summary);
    config.base_instructions = Some(base_instructions.text.clone());
    config.base_instructions_provenance = base_instructions.provenance.clone();
    Ok(config)
}

pub(crate) fn build_agent_resume_config(turn: &TurnContext) -> Result<Config, String> {
    let mut config = build_agent_shared_config(turn)?;
    // For resume, keep base instructions sourced from rollout/session metadata.
    config.base_instructions = None;
    config.base_instructions_provenance = None;
    Ok(config)
}

fn build_agent_shared_config(turn: &TurnContext) -> Result<Config, String> {
    let base_config = turn.config.clone();
    let mut config = (*base_config).clone();
    // Preserve activation for history forks without freezing the parent's model-owned prompts.
    // Fresh child startup restores configured preferences from the retained snapshot.
    config.token_budget = turn.configured_token_budget.clone();
    config.model = Some(turn.model_info().slug.clone());
    config.model_provider = turn.provider.info().clone();
    config.model_reasoning_effort = turn
        .reasoning_effort()
        .or(turn.model_info().default_reasoning_level.as_ref())
        .cloned();
    config.model_reasoning_summary = Some(turn.reasoning_summary());
    config.developer_instructions = turn.developer_instructions.clone();
    if turn.multi_agent_version == MultiAgentVersion::V2
        && let Some(developer_instructions) = turn
            .config
            .multi_agent_v2
            .subagent_developer_instructions
            .clone()
    {
        config.developer_instructions = Some(developer_instructions);
    }
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    Ok(config)
}

fn reject_full_fork_agent_type_override(agent_type: Option<&str>) -> Result<(), String> {
    if agent_type.is_some() {
        return Err(
            "Full-history forked agents inherit the parent agent type; omit agent_type, or spawn without a full-history fork.".to_string());
    }
    Ok(())
}

/// Copies runtime-only turn state onto a child config before it is handed to `AgentControl`.
///
/// These values are chosen by the live turn rather than persisted config, so leaving them stale can
/// make a child agent disagree with its parent about approval policy, cwd, or sandboxing.
fn apply_spawn_agent_runtime_overrides(
    config: &mut Config,
    turn: &TurnContext,
) -> Result<(), String> {
    config
        .permissions
        .approval_policy
        .set(turn.approval_policy())
        .map_err(|err| format!("approval_policy is invalid: {err}"))?;
    config.approvals_reviewer = turn.config.approvals_reviewer;
    #[allow(deprecated)]
    let turn_cwd = turn.cwd.clone();
    config.cwd = turn_cwd;
    config
        .permissions
        .set_permission_profile_from_session_snapshot(
            turn.config
                .permissions
                .permission_profile_state()
                .snapshot(),
        )
        .map_err(|err| format!("permission_profile is invalid: {err}"))?;
    Ok(())
}

async fn apply_requested_spawn_agent_model_overrides(
    session: &Session,
    turn: &TurnContext,
    settings: &ResolvedStepSettings,
    config: &mut Config,
    requested_model: Option<&str>,
    requested_reasoning_effort: Option<ReasoningEffort>,
    fork_mode: Option<&SpawnAgentForkMode>,
) -> Result<(), String> {
    let requested_model = requested_model.or(turn.config.agent_default_subagent_model.as_deref());
    let requested_reasoning_effort = requested_reasoning_effort
        .or_else(|| turn.config.agent_default_subagent_reasoning_effort.clone());
    if requested_model.is_none() && requested_reasoning_effort.is_none() {
        return Ok(());
    }

    if let Some(requested_model) = requested_model {
        let catalogs = session
            .agent_model_catalogs(config, RefreshStrategy::OnlineIfUncached)
            .await;
        let (catalog, selected_model_name) =
            find_spawn_agent_model(&catalogs, requested_model, turn.multi_agent_version)?;
        if catalog.provider_id != config.model_provider_id {
            if fork_mode.is_some() {
                return Err("History-forked agents must retain the parent model provider; use fork_turns=none for a different provider.".to_owned());
            }
            config.model_provider_id.clone_from(&catalog.provider_id);
            config.model_provider = config.model_providers[&catalog.provider_id].clone();
            config.model_catalog = None;
            if matches!(
                config.base_instructions_provenance,
                Some(BaseInstructionsProvenance::Model { .. })
            ) {
                config.base_instructions = None;
                config.base_instructions_provenance = None;
            }
        }
        let selected_model_info = catalog
            .manager
            .get_model_info(selected_model_name, &config.to_models_manager_config())
            .await;

        config.model = Some(selected_model_name.to_owned());
        if let Some(reasoning_effort) = requested_reasoning_effort {
            validate_spawn_agent_reasoning_effort(
                selected_model_name,
                &selected_model_info.supported_reasoning_levels,
                &reasoning_effort,
            )?;
            config.model_reasoning_effort = Some(reasoning_effort);
        } else {
            config.model_reasoning_effort = selected_model_info.default_reasoning_level;
        }

        return Ok(());
    }

    if let Some(reasoning_effort) = requested_reasoning_effort {
        validate_spawn_agent_reasoning_effort(
            &settings.model_info.slug,
            &settings.model_info.supported_reasoning_levels,
            &reasoning_effort,
        )?;
        config.model_reasoning_effort = Some(reasoning_effort);
    }

    Ok(())
}

pub(crate) async fn apply_spawn_agent_service_tier(
    session: &Session,
    config: &mut Config,
    requested: Option<&str>,
) -> Result<(), String> {
    config.service_tier = None;
    if let Some(requested) = requested {
        let model = config.model.as_deref().ok_or_else(|| {
            "spawn_agent could not resolve the child model for service tier validation".to_owned()
        })?;
        let model_info = session
            .agent_models_manager(config)
            .await?
            .get_model_info(model, &config.to_models_manager_config())
            .await;
        config.service_tier = normalize_agent_service_tier(
            requested,
            &model_info,
            config.features.enabled(codex_features::Feature::FastMode),
        )?;
        return Ok(());
    }
    Ok(())
}

async fn apply_spawn_agent_role(
    session: &Session,
    config: &mut Config,
    role_name: Option<&str>,
) -> Result<(), String> {
    let previous_model = config.model.clone();
    let previous_reasoning_effort = config.model_reasoning_effort.clone();
    apply_role_to_config(config, role_name).await?;
    if config.model == previous_model && config.model_reasoning_effort == previous_reasoning_effort
    {
        return Ok(());
    }

    let Some(reasoning_effort) = config.model_reasoning_effort.clone() else {
        return Ok(());
    };
    let model = config.model.clone().ok_or_else(|| {
        "spawn_agent could not resolve the child model for reasoning effort validation".to_string()
    })?;
    let model_info = session
        .agent_models_manager(config)
        .await?
        .get_model_info(&model, &config.to_models_manager_config())
        .await;
    if model_info.used_fallback_model_metadata {
        return Ok(());
    }

    validate_spawn_agent_reasoning_effort(
        &model,
        &model_info.supported_reasoning_levels,
        &reasoning_effort,
    )
}

fn find_spawn_agent_model<'a>(
    catalogs: &'a [ProviderModels],
    requested_model: &str,
    multi_agent_version: MultiAgentVersion,
) -> Result<(&'a ProviderModels, &'a str), String> {
    let mut matches = catalogs
        .iter()
        .flat_map(|catalog| catalog.models.iter().map(move |model| (catalog, model)))
        .filter(|(catalog, model)| {
            (model.model == requested_model
                || requested_model.strip_prefix(&format!("{}/", catalog.provider_id))
                    == Some(model.model.as_str()))
                && model_supports_multi_agent_backend(model, multi_agent_version)
        });
    match (matches.next(), matches.next()) {
        (Some((catalog, model)), None) => Ok((catalog, model.model.as_str())),
        (Some(_), Some(_)) => Err(format!(
            "Model `{requested_model}` is available from multiple providers; use a provider/model selector."
        )),
        (None, _) => {
            let available =
                crate::agent::model_catalog::advertised_agent_models(catalogs, multi_agent_version)
                    .into_iter()
                    .map(|model| model.model)
                    .collect::<Vec<_>>()
                    .join(", ");
            Err(format!(
                "Unknown model `{requested_model}` for spawn_agent. Available models: {available}"
            ))
        }
    }
}

fn validate_spawn_agent_reasoning_effort(
    model: &str,
    supported_reasoning_levels: &[ReasoningEffortPreset],
    requested_reasoning_effort: &ReasoningEffort,
) -> Result<(), String> {
    if supported_reasoning_levels
        .iter()
        .any(|preset| &preset.effort == requested_reasoning_effort)
    {
        return Ok(());
    }

    let supported = supported_reasoning_levels
        .iter()
        .map(|preset| preset.effort.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "Reasoning effort `{requested_reasoning_effort}` is not supported for model `{model}`. Supported reasoning efforts: {supported}"
    ))
}
