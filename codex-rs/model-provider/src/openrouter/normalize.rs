use super::ResolvedModel;
use codex_models_manager::bundled_models_response;
use codex_models_manager::model_info::model_info_with_local_defaults;
use codex_protocol::EndpointFilter;
use codex_protocol::ModelSlug;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelServiceTier;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use std::sync::OnceLock;

pub(super) fn local(slug: &str) -> ModelInfo {
    let mut info = model_info_with_local_defaults(slug);
    info.visibility = ModelVisibility::List;
    info.supports_reasoning_summary_parameter = slug.starts_with("openai/");
    info
}

pub fn normalize(resolved: &ResolvedModel, filter: &EndpointFilter, priority: i32) -> ModelInfo {
    let ResolvedModel {
        slug,
        model,
        endpoints,
    } = resolved;
    let mut info = local(slug.as_str());
    if let Some(suffix) = model.id.as_str().strip_prefix("openai/") {
        static BUNDLED: OnceLock<Vec<ModelInfo>> = OnceLock::new();
        let bundled = BUNDLED.get_or_init(|| match bundled_models_response() {
            Ok(response) => response.models,
            Err(error) => panic!("bundled OpenAI metadata is invalid: {error}"),
        });
        if let Some(shape) = bundled.iter().find(|info| info.slug == suffix) {
            info.model_messages = shape.model_messages.clone();
            info.tool_mode = shape.tool_mode;
            info.apply_patch_tool_type = shape.apply_patch_tool_type.clone();
            info.shell_type = shape.shell_type;
            info.truncation_policy = shape.truncation_policy;
            info.include_skills_usage_instructions = shape.include_skills_usage_instructions;
            info.include_plugin_usage_instructions = shape.include_plugin_usage_instructions;
            info.include_apps_usage_instructions = shape.include_apps_usage_instructions;
        }
    }
    info.display_name = match slug {
        ModelSlug::Preset(value) => value.strip_prefix("@preset/").unwrap_or(value).to_owned(),
        ModelSlug::Model(_) => model
            .name
            .split_once(": ")
            .map_or(model.name.as_str(), |(_, name)| name)
            .to_owned(),
    };
    let allowed = endpoints
        .iter()
        .filter(|endpoint| filter.admits(endpoint.tag.as_str()))
        .collect::<Vec<_>>();
    let context = allowed
        .iter()
        .map(|endpoint| endpoint.context_length)
        .max()
        .unwrap_or(model.context_length);
    info.context_window = Some(context);
    info.max_context_window = Some(context);
    let price = |price: Option<super::Price>| {
        price.map_or_else(|| "?".to_owned(), |price| price.per_mtok().to_string())
    };
    let mut description = format!(
        "{} in · {} out per M · {} context",
        price(model.pricing.prompt),
        price(model.pricing.completion),
        context_label(context)
    );
    if matches!(slug, ModelSlug::Preset(_)) {
        description.push_str(&format!(" · preset for {}", model.id));
    }
    if let Some(date) = &model.expiration_date {
        description.push_str(&format!(" · retires {date}"));
    }
    if !model.supports_tools() {
        description.push_str(" · no tool calling");
    }
    info.description = Some(description);
    if let Some(reasoning) = &model.reasoning {
        let mut efforts = reasoning.supported_efforts.clone();
        efforts.sort_by_key(effort_rank);
        efforts.dedup();
        if reasoning.mandatory {
            efforts.retain(|effort| *effort != ReasoningEffort::None);
        }
        info.supported_reasoning_levels = efforts
            .into_iter()
            .map(|effort| ReasoningEffortPreset {
                description: effort_description(&effort).to_owned(),
                effort,
            })
            .collect();
        info.default_reasoning_level = reasoning.default_effort.clone().filter(|effort| {
            info.supported_reasoning_levels
                .iter()
                .any(|preset| &preset.effort == effort)
        });
    }
    info.service_tiers = [
        ("priority", "Fast"),
        ("flex", "Flex"),
        ("ultrafast", "Ultra Fast"),
    ]
    .into_iter()
    .filter(|(tier, _)| {
        allowed
            .iter()
            .any(|endpoint| endpoint.service_tier.as_deref() == Some(tier))
    })
    .map(|(id, name)| ModelServiceTier {
        id: id.to_owned(),
        name: name.to_owned(),
        description: name.to_owned(),
    })
    .collect();
    info.support_verbosity = model
        .supported_parameters
        .iter()
        .any(|parameter| parameter == "verbosity");
    info.supports_reasoning_summary_parameter = model.id.as_str().starts_with("openai/");
    info.input_modalities = [
        ("text", InputModality::Text),
        ("image", InputModality::Image),
        ("audio", InputModality::Audio),
    ]
    .into_iter()
    .filter(|(name, _)| {
        model
            .architecture
            .input_modalities
            .iter()
            .any(|modality| modality == name)
    })
    .map(|(_, modality)| modality)
    .collect();
    info.priority = priority;
    info.used_fallback_model_metadata = false;
    info
}

fn context_label(context: i64) -> String {
    if context >= 1_000_000 {
        format!("{}M", compact(context as f64 / 1_048_576.0))
    } else if context >= 1_000 {
        format!("{}K", compact(context as f64 / 1_024.0))
    } else {
        context.to_string()
    }
}

fn compact(value: f64) -> String {
    format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn effort_rank(effort: &ReasoningEffort) -> u8 {
    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::XHigh => 5,
        ReasoningEffort::Max => 6,
        ReasoningEffort::Ultra => 7,
        ReasoningEffort::Persistent => 8,
        ReasoningEffort::Custom(_) => 9,
    }
}

fn effort_description(effort: &ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "No reasoning",
        ReasoningEffort::Minimal => "Minimal reasoning",
        ReasoningEffort::Low => "Light reasoning",
        ReasoningEffort::Medium => "Balanced reasoning",
        ReasoningEffort::High => "Thorough reasoning",
        ReasoningEffort::XHigh => "Extra thorough reasoning",
        ReasoningEffort::Max => "Maximum reasoning",
        ReasoningEffort::Ultra => "Ultra reasoning",
        ReasoningEffort::Persistent => "Persistent reasoning",
        ReasoningEffort::Custom(_) => "Model-defined reasoning",
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
