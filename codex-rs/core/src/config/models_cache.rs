use super::Config;
use std::path::PathBuf;

impl Config {
    /// The file that caches the configured provider's model catalog.
    pub fn models_cache_path(&self) -> PathBuf {
        self.codex_home
            .join(codex_model_provider_info::models_cache_file_name(
                &self.model_provider_id,
            ))
            .to_path_buf()
    }
}
