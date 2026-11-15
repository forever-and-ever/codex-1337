use codex_api::ApiError;
use codex_models_manager::model_info::model_info_with_local_defaults;
use codex_protocol::openai_models::AnthropicModelCapabilities;
use codex_protocol::openai_models::AnthropicThinkingMode;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::future::Future;
use std::num::NonZeroU32;
const MAX_MODELS: usize = 1000;

#[derive(Deserialize)]
pub(crate) struct Page {
    data: Vec<NativeModel>,
    has_more: bool,
    last_id: Option<String>,
}

#[derive(Deserialize)]
struct NativeModel {
    id: String,
    display_name: String,
    max_input_tokens: Option<NonZeroU32>,
    max_tokens: Option<NonZeroU32>,
    capabilities: Option<Capabilities>,
}

#[derive(Default, Deserialize)]
struct Supported {
    #[serde(default)]
    supported: bool,
}

#[derive(Default, Deserialize)]
struct Thinking {
    #[serde(flatten)]
    support: Supported,
    #[serde(default)]
    types: ThinkingTypes,
}

#[derive(Default, Deserialize)]
struct ThinkingTypes {
    #[serde(default)]
    enabled: Supported,
    #[serde(default)]
    adaptive: Supported,
}

#[derive(Default, Deserialize)]
struct Compaction {
    #[serde(flatten)]
    support: Supported,
    #[serde(default)]
    summarize: Supported,
}

#[derive(Default, Deserialize)]
struct Effort {
    #[serde(flatten)]
    support: Supported,
    #[serde(default)]
    low: Supported,
    #[serde(default)]
    medium: Supported,
    #[serde(default)]
    high: Supported,
    #[serde(default)]
    xhigh: Supported,
    #[serde(default)]
    max: Supported,
}

#[derive(Deserialize)]
struct Capabilities {
    #[serde(default)]
    thinking: Thinking,
    #[serde(default)]
    effort: Effort,
    #[serde(default)]
    compaction: Compaction,
    #[serde(default)]
    structured_outputs: Supported,
    #[serde(default)]
    image_input: Supported,
}

pub(crate) async fn list_models<Fut, E>(
    mut fetch_page: impl FnMut(Option<String>) -> Fut,
) -> Result<(Vec<ModelInfo>, Option<String>), E>
where
    Fut: Future<Output = Result<Page, E>> + Send,
    E: From<ApiError>,
{
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for _ in 0..10 {
        let page = fetch_page(cursor).await?;
        for native in page.data {
            if !seen.insert(native.id.clone()) || seen.len() > MAX_MODELS {
                return Err(
                    invalid("native model catalogue repeats IDs or exceeds its limit").into(),
                );
            }
            if let Some(mut model) = project(native)? {
                model.priority = i32::try_from(models.len())
                    .map_err(|_| invalid("native model catalogue exceeds its priority range"))?;
                models.push(model);
            }
        }
        if !page.has_more {
            // ETags describe individual pages, not the aggregate catalogue.
            return Ok((models, None));
        }
        let last_id = page
            .last_id
            .filter(|id| seen.contains(id))
            .ok_or_else(|| invalid("native model catalogue lacks a valid next cursor"))?;
        cursor = Some(last_id);
    }
    Err(invalid("native model catalogue exceeds its page limit").into())
}

fn project(native: NativeModel) -> Result<Option<ModelInfo>, ApiError> {
    let NativeModel {
        id,
        display_name,
        max_input_tokens,
        max_tokens,
        capabilities,
    } = native;
    if !id.starts_with("claude-") || id.contains('/') {
        return Err(invalid(
            "native model catalogue contains an invalid model identity",
        ));
    }
    let (Some(input_limit), Some(output_limit), Some(capabilities)) =
        (max_input_tokens, max_tokens, capabilities)
    else {
        // Null metadata cannot establish an executable model's capabilities.
        return Ok(None);
    };
    let thinking = if !capabilities.thinking.support.supported {
        AnthropicThinkingMode::Disabled
    } else if capabilities.thinking.types.adaptive.supported {
        AnthropicThinkingMode::Adaptive
    } else if capabilities.thinking.types.enabled.supported {
        AnthropicThinkingMode::Manual
    } else {
        return Err(invalid(
            "native model advertises no supported thinking strategy",
        ));
    };
    let supported_efforts = if capabilities.effort.support.supported {
        [
            (ReasoningEffort::Low, capabilities.effort.low.supported),
            (
                ReasoningEffort::Medium,
                capabilities.effort.medium.supported,
            ),
            (ReasoningEffort::High, capabilities.effort.high.supported),
            (ReasoningEffort::XHigh, capabilities.effort.xhigh.supported),
            (ReasoningEffort::Max, capabilities.effort.max.supported),
        ]
        .into_iter()
        .filter_map(|(effort, supported)| supported.then_some(effort))
        .collect()
    } else {
        Vec::new()
    };
    let native_capabilities = AnthropicModelCapabilities {
        thinking,
        max_output_tokens: output_limit,
        supported_efforts,
        supports_structured_outputs: capabilities.structured_outputs.supported,
    };
    // /models does not advertise disabled thinking. Unknown models do not inherit it.
    // https://platform.claude.com/docs/en/build-with-claude/thinking
    let supports_disabled_thinking = matches!(
        id.as_str(),
        "claude-opus-5"
            | "claude-sonnet-5"
            | "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-sonnet-4-6"
            | "claude-opus-4-5-20251101"
            | "claude-haiku-4-5-20251001"
            | "claude-sonnet-4-5-20250929"
    );
    let mut efforts = if supports_disabled_thinking || thinking == AnthropicThinkingMode::Disabled {
        vec![ReasoningEffort::None]
    } else {
        Vec::new()
    };
    match thinking {
        AnthropicThinkingMode::Disabled => {}
        AnthropicThinkingMode::Adaptive => {
            efforts.extend(native_capabilities.supported_efforts.iter().cloned());
        }
        AnthropicThinkingMode::Manual => efforts.extend(
            [
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
                ReasoningEffort::Max,
            ]
            .into_iter()
            .filter(|effort| native_capabilities.manual_thinking_budget(effort).is_some()),
        ),
    }
    let mut model = model_info_with_local_defaults(&id);
    model.display_name = display_name;
    model.context_window = Some(i64::from(input_limit.get()));
    model.max_context_window = model.context_window;
    model.visibility = ModelVisibility::List;
    model.priority = 0;
    model.supports_reasoning_summary_parameter = false;
    model.supported_reasoning_levels = efforts
        .into_iter()
        .map(|effort| ReasoningEffortPreset {
            description: effort.as_str().to_owned(),
            effort,
        })
        .collect();
    model.default_reasoning_level = model
        .supported_reasoning_levels
        .iter()
        .find(|preset| preset.effort == ReasoningEffort::Medium)
        .map(|preset| preset.effort.clone());
    model.supports_native_compaction =
        capabilities.compaction.support.supported && capabilities.compaction.summarize.supported;
    model.input_modalities = vec![InputModality::Text];
    if capabilities.image_input.supported {
        model.input_modalities.push(InputModality::Image);
    }
    model.anthropic = Some(native_capabilities);
    Ok(Some(crate::claude_catalog::normalize_model_info(
        model,
        codex_models_manager::ModelOrigin::Provider,
    )))
}

fn invalid(message: &str) -> ApiError {
    ApiError::InvalidRequest {
        message: message.to_owned(),
    }
}

#[cfg(test)]
#[path = "native_claude_catalog_tests.rs"]
mod tests;

pub(crate) enum PageError {
    Api(ApiError),
    Auth(codex_protocol::error::CodexErr),
}

impl From<ApiError> for PageError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl PageError {
    pub(crate) fn into_core(
        self,
        map: impl FnOnce(ApiError) -> codex_protocol::error::CodexErr,
    ) -> codex_protocol::error::CodexErr {
        match self {
            Self::Api(error) => map(error),
            Self::Auth(error) => error,
        }
    }
}
