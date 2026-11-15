//! Copies only identity-bound provider publications, never bundled fallback metadata.

use crate::cache::ModelsCacheEntry;
use chrono::DateTime;
use chrono::Utc;

use super::CatalogLoadMode;
use super::ModelsManagerFuture;
use super::OpenAiModelsManager;
use super::RefreshStrategy;

pub struct LiveModelCatalog(ModelsCacheEntry);

impl OpenAiModelsManager {
    pub(super) fn snapshot_live_catalog(
        &self,
    ) -> ModelsManagerFuture<'_, Option<LiveModelCatalog>> {
        Box::pin(async move {
            let current = self.remote_models.read().await;
            if !self.catalog_is_initialized(&current)
                || current.entry.fetched_at == DateTime::<Utc>::MIN_UTC
            {
                return None;
            }
            let mut entry = current.entry.clone();
            entry
                .models
                .retain(|model| current.provider_model_ids.contains(&model.slug));
            Some(LiveModelCatalog(entry))
        })
    }

    pub(super) fn apply_live_catalog(
        &self,
        publication: LiveModelCatalog,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async move {
            if self.api_key_discovery_disabled() {
                return;
            }
            let mut current = self.remote_models.write().await;
            // Preserve the same authoritative-denial fence as cache adoption.
            if current.entry.fetched_at == DateTime::<Utc>::MIN_UTC
                && current.entry.identity == self.endpoint_client.identity()
            {
                return;
            }
            self.apply_remote_models(
                &mut current,
                publication.0,
                CatalogLoadMode::Refresh(RefreshStrategy::InMemory),
            );
        })
    }
}
