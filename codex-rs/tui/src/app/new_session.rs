//! New-session configuration from server defaults and explicit launch settings.
//!
//! Build the replacement configuration without replacing the active task's settings.

use super::*;
use codex_config::ConfigLayerSource;

pub(crate) fn has_launch_setting(
    config: &Config,
    cli_kv_overrides: &[(String, TomlValue)],
    key: &str,
) -> bool {
    // A remote server cannot resolve this invocation's explicitly selected local profile.
    // Only count the profile when it supplies the effective value; project settings can shadow it.
    cli_kv_overrides.iter().any(|(path, _)| path == key)
        || config
            .config_layer_stack
            .layers_high_to_low()
            .find(|layer| layer.config.get(key).is_some())
            .is_some_and(|layer| {
                matches!(
                    layer.name,
                    ConfigLayerSource::User {
                        profile: Some(_),
                        ..
                    }
                )
            })
}

pub(super) fn overlay_new_session_defaults(
    config: &mut Config,
    defaults: &codex_app_server_protocol::Config,
    cli_kv_overrides: &[(String, TomlValue)],
    harness_overrides: &ConfigOverrides,
) {
    if harness_overrides.model.is_none() && !has_launch_setting(config, cli_kv_overrides, "model") {
        config.model = defaults.model.clone();
    }
    if !has_launch_setting(config, cli_kv_overrides, "model_reasoning_effort") {
        config.model_reasoning_effort = defaults.model_reasoning_effort.clone();
    }
    if !has_launch_setting(config, cli_kv_overrides, "daybreak") {
        config.daybreak_enabled = defaults
            .additional
            .get("daybreak")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    }
}

impl App {
    pub(super) fn project_new_session_config(
        &mut self,
        app_server: &AppServerSession,
        projected: (Config, crate::local_settings::LocalSettings),
        defaults: Option<crate::config_update::EffectiveConfig>,
    ) -> Result<(Config, crate::local_settings::LocalSettings)> {
        let (mut config, local_settings) = projected;
        if let Some(defaults) = defaults.as_ref() {
            crate::projectless::apply_defaults(
                &mut config,
                &self.harness_overrides,
                app_server,
                &self.environment_manager,
                defaults,
            );
        }
        self.apply_runtime_policy_overrides(&mut config, RuntimePolicyOverrideScope::ExplicitOnly)?;
        config.service_tier = self.chat_widget.configured_service_tier();
        if let Some(defaults) = defaults.as_ref() {
            overlay_new_session_defaults(
                &mut config,
                &defaults.config,
                &self.cli_kv_overrides,
                &self.harness_overrides,
            );
        }
        Ok((config, local_settings))
    }
}
