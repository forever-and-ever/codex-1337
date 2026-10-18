//! Move-only configuration work carried by navigation receipts.

use super::*;
use crate::local_settings::LocalSettings;

pub(super) struct ConfigRebuild {
    builder: ConfigBuilder,
    network: codex_http_client::NetworkPolicy,
    cwd: PathBuf,
    settings: LocalSettings,
    fallback: Option<Box<Config>>,
}

impl ConfigRebuild {
    pub(super) async fn run(self) -> Result<Box<(Config, LocalSettings)>> {
        let config = match config_persistence::build_config_on_runtime_worker(
            self.builder,
            self.network,
            format!("Failed to rebuild config for cwd {}", self.cwd.display()),
        )
        .await
        {
            Ok(config) => config,
            Err(error) => match self.fallback {
                Some(config) => {
                    tracing::warn!(%error, cwd = %self.cwd.display(),
                        "failed to rebuild config for same-cwd resume; using current in-memory config");
                    return Ok(Box::new((*config, self.settings)));
                }
                None => return Err(error),
            },
        };
        let settings = self.settings.reloaded(&config);
        Ok(Box::new((config, settings)))
    }
}

impl App {
    pub(super) fn capture_config_rebuild(
        &self,
        cwd: PathBuf,
        current_cwd: Option<&Path>,
    ) -> Box<ConfigRebuild> {
        Box::new(ConfigRebuild {
            builder: self.config_builder_for_cwd(cwd.clone()),
            network: self.config.application_network_policy.clone(),
            fallback: current_cwd
                .is_some_and(|current| !crate::session_resume::cwds_differ(current, &cwd))
                .then(|| Box::new(self.config.clone())),
            cwd,
            settings: self.local_settings.clone(),
        })
    }

    pub(super) fn apply_navigation_config_refresh(
        &mut self,
        result: Result<Box<(Config, LocalSettings)>>,
        action: &str,
    ) {
        if let Err(error) = result.and_then(|projected| self.install_refreshed_config(projected.0))
        {
            tracing::warn!(%error, action,
                "failed to refresh config before thread transition; continuing with current in-memory config");
        }
    }
}
