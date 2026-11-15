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
use std::fmt::Write as _;
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
        let bundled = BUNDLED.get_or_init(|| {
            bundled_models_response()
                .map(|response| response.models)
                .unwrap_or_default()
        });
        if let Some(shape) = bundled.iter().find(|info| info.slug == suffix) {
            info.model_messages.clone_from(&shape.model_messages);
            info.tool_mode = shape.tool_mode;
            info.apply_patch_tool_type
                .clone_from(&shape.apply_patch_tool_type);
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
        price(
            allowed
                .iter()
                .filter_map(|endpoint| endpoint.pricing.prompt)
                .min_by_key(|price| price.per_mtok())
                .or(model.pricing.prompt)
        ),
        price(
            allowed
                .iter()
                .filter_map(|endpoint| endpoint.pricing.completion)
                .min_by_key(|price| price.per_mtok())
                .or(model.pricing.completion)
        ),
        context_label(context)
    );
    if matches!(slug, ModelSlug::Preset(_)) {
        let _ = write!(description, " · preset for {}", model.id);
    }
    if let Some(date) = &model.expiration_date {
        let _ = write!(description, " · retires {date}");
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
    info
}

/// "1M", "256K", "200K": the first of decimal or binary units that divides the size exactly,
/// otherwise decimal units to two places.
#[expect(
    clippy::cast_precision_loss,
    reason = "A two-decimal context label intentionally rounds token counts; model limits retain their exact integer values"
)]
fn context_label(tokens: i64) -> String {
    let exact = |base: i64| {
        let (unit, suffix) = if tokens >= base * base {
            (base * base, "M")
        } else {
            (base, "K")
        };
        (tokens >= base && tokens % unit == 0).then(|| format!("{}{suffix}", tokens / unit))
    };
    exact(1_000)
        .or_else(|| exact(1_024))
        .unwrap_or_else(|| match tokens {
            1_000_000.. => format!("{}M", compact(tokens as f64 / 1e6)),
            1_000.. => format!("{}K", compact(tokens as f64 / 1e3)),
            _ => tokens.to_string(),
        })
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
