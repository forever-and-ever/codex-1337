use codex_protocol::EndpointFilter;
use codex_protocol::KeyName;
use codex_protocol::ModelSlug;
use codex_utils_absolute_path::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde::de::Error;
use std::collections::BTreeMap;
use std::path::Path;

pub enum OpenRouterModelPreference<'a> {
    Saved(&'a EndpointFilter),
    Forgotten(&'a EndpointFilter),
    Unlisted,
}

impl OpenRouterModelPreference<'_> {
    pub fn endpoint_filter(self) -> EndpointFilter {
        match self {
            Self::Saved(filter) | Self::Forgotten(filter) => filter.clone(),
            Self::Unlisted => EndpointFilter::All,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(default, deny_unknown_fields)]
pub struct OpenRouterConfig {
    pub default_key: Option<KeyName>,
    pub key_files: BTreeMap<KeyName, AbsolutePathBuf>,
    pub models: BTreeMap<ModelSlug, EndpointFilter>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    forgotten_models: BTreeMap<ModelSlug, EndpointFilter>,
}

impl OpenRouterConfig {
    pub fn default_key(&self) -> Option<&KeyName> {
        match &self.default_key {
            Some(key) => Some(key),
            None if self.key_files.len() == 1 => self.key_files.keys().next(),
            None => None,
        }
    }

    pub fn model_preference(&self, slug: &ModelSlug) -> OpenRouterModelPreference<'_> {
        if let Some(filter) = self.models.get(slug) {
            OpenRouterModelPreference::Saved(filter)
        } else if let Some(filter) = self.forgotten_models.get(slug) {
            OpenRouterModelPreference::Forgotten(filter)
        } else {
            OpenRouterModelPreference::Unlisted
        }
    }
}

impl<'de> Deserialize<'de> for OpenRouterConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct Input {
            default_key: Option<KeyName>,
            key_files: BTreeMap<KeyName, String>,
            models: BTreeMap<ModelSlug, EndpointFilter>,
            forgotten_models: BTreeMap<ModelSlug, EndpointFilter>,
        }
        let Input {
            default_key,
            key_files,
            models,
            forgotten_models,
        } = Input::deserialize(deserializer)?;
        if default_key
            .as_ref()
            .is_some_and(|key| !key_files.contains_key(key))
        {
            return Err(D::Error::custom(
                "OpenRouter default_key must name a key_files entry",
            ));
        }
        if models.iter().any(|(slug, filter)| {
            matches!(slug, ModelSlug::Preset(_)) && !matches!(filter, EndpointFilter::All)
        }) {
            return Err(D::Error::custom(
                "OpenRouter presets own their routing and accept only {}",
            ));
        }
        if forgotten_models.iter().any(|(slug, filter)| {
            models.contains_key(slug)
                || matches!(slug, ModelSlug::Preset(_))
                || matches!(filter, EndpointFilter::All)
        }) {
            return Err(D::Error::custom(
                "OpenRouter forgotten_models must contain only restricted, unsaved models",
            ));
        }
        let key_files = key_files
            .into_iter()
            .map(|(name, path)| {
                if path.starts_with("sk-or-") {
                    return Err(D::Error::custom("store the key's file path, not the key"));
                }
                AbsolutePathBuf::from_absolute_path(path)
                    .map(|path| (name, path))
                    .map_err(D::Error::custom)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            default_key,
            key_files,
            models,
            forgotten_models,
        })
    }
}

pub async fn read_openrouter_config(codex_home: &Path) -> std::io::Result<OpenRouterConfig> {
    Ok(crate::read_account_preferences(codex_home)
        .await?
        .unwrap_or_default()
        .openrouter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn endpoint_filter_serde() {
        for value in [
            json!({}),
            json!({"only": ["deepinfra/turbo", "deepseek"]}),
            json!({"ignore": ["chutes"]}),
        ] {
            let filter: EndpointFilter =
                serde_json::from_value(value.clone()).expect("accepted filter");
            assert_eq!(
                serde_json::to_value(filter).expect("serialize filter"),
                value
            );
        }
        for value in [
            json!({"only": []}),
            json!({"ignore": []}),
            json!({"only": ["deepseek"], "ignore": ["chutes"]}),
            json!({"unknown": ["deepseek"]}),
            json!({"only": ["DeepSeek"]}),
            json!({"only": null}),
            json!([]),
        ] {
            assert!(
                serde_json::from_value::<EndpointFilter>(value.clone()).is_err(),
                "accepted invalid filter: {value}"
            );
        }
        let config: OpenRouterConfig = toml::from_str(
            "[models]\n'@preset/example' = {}\n'author/model' = { only = ['host/region'] }",
        )
        .expect("preset and model config");
        assert_eq!(
            config.models[&"@preset/example".parse::<ModelSlug>().expect("preset slug")],
            EndpointFilter::All
        );
        // A slug has one route even while absent from the picker.
        assert!(
            toml::from_str::<OpenRouterConfig>(
                "[models]\n'author/model' = {}\n[forgotten_models]\n'author/model' = { only = ['host'] }"
            )
            .is_err()
        );
        for filter in ["{ only = ['host'] }", "{ ignore = ['host'] }"] {
            assert!(
                toml::from_str::<OpenRouterConfig>(&format!(
                    "[models]\n'@preset/example' = {filter}"
                ))
                .is_err()
            );
        }
    }
}
