use super::Client;
use super::credentials;
use super::normalize;
use super::normalize::local;
use codex_config::OpenRouterConfig;
use codex_config::read_openrouter_config;
use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::CatalogModel;
use codex_models_manager::ModelOrigin;
use codex_models_manager::ModelsManagerConfig;
use codex_models_manager::collaboration_mode_presets::builtin_collaboration_mode_presets;
use codex_models_manager::manager::ModelsManager;
use codex_models_manager::manager::ModelsManagerFuture;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_models_manager::manager::StaticModelsManager;
use codex_models_manager::model_info::with_config_overrides;
use codex_protocol::EndpointFilter;
use codex_protocol::ModelSlug;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use futures::StreamExt;
use futures::stream;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio::sync::TryLockError;

#[derive(Debug)]
struct Snapshot {
    identity: BTreeMap<ModelSlug, EndpointFilter>,
    models: Vec<ModelInfo>,
    fetched_at: Instant,
}

#[derive(Debug)]
struct Catalog {
    home: PathBuf,
    client: Result<Client, super::Error>,
    snapshot: RwLock<Option<Snapshot>>,
    refresh: tokio::sync::Semaphore,
}

pub(crate) fn catalog_for_provider(
    info: &ModelProviderInfo,
    auth: Option<&Arc<AuthManager>>,
) -> SharedModelsManager {
    let Some(auth) = auth else {
        return Arc::new(StaticModelsManager::new(
            None,
            ModelsResponse { models: Vec::new() },
            ModelOrigin::Provider,
        ));
    };
    let runtime = auth.runtime_config();
    shared_catalog(
        &runtime.codex_home,
        info.base_url
            .as_deref()
            .unwrap_or("https://openrouter.ai/api/v1"),
        runtime.auth_route_config.http_client_factory().clone(),
    )
}

pub fn shared_catalog(
    home: &Path,
    base_url: &str,
    factory: HttpClientFactory,
) -> SharedModelsManager {
    type Entry = ((PathBuf, String), Arc<Catalog>);
    static CATALOGS: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
    let mut catalogs = CATALOGS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (home.to_owned(), base_url.trim_end_matches('/').to_owned());
    if let Some((_, catalog)) = catalogs.iter().find(|(candidate, _)| *candidate == key) {
        return Arc::clone(catalog) as SharedModelsManager;
    }
    let catalog = Arc::new(Catalog {
        home: home.to_owned(),
        client: Client::new(base_url, factory),
        snapshot: RwLock::new(None),
        refresh: tokio::sync::Semaphore::new(1),
    });
    catalogs.push((key, Arc::clone(&catalog)));
    catalog
}

impl Catalog {
    async fn models(&self) -> Vec<ModelInfo> {
        let _permit = self
            .refresh
            .acquire()
            .await
            .unwrap_or_else(|_| unreachable!("catalog refresh semaphore is never closed"));
        let Ok(preferences) = read_openrouter_config(&self.home).await else {
            tracing::warn!("Cannot read OpenRouter model preferences");
            return Vec::new();
        };
        if let Some(snapshot) = self.snapshot.read().await.as_ref()
            && snapshot.identity == preferences.models
            && snapshot.fetched_at.elapsed() < Duration::from_mins(5)
        {
            return snapshot.models.clone();
        }
        let models = self.fetch(&preferences).await;
        *self.snapshot.write().await = Some(Snapshot {
            identity: preferences.models,
            models: models.clone(),
            fetched_at: Instant::now(),
        });
        models
    }

    async fn fetch(&self, preferences: &OpenRouterConfig) -> Vec<ModelInfo> {
        let bearer = match preferences.default_key() {
            Some(key) => credentials(&self.home, key).await.map(Some),
            None => Ok(None),
        };
        let admitted = match (&self.client, &bearer) {
            (Ok(client), Ok(Some(bearer))) if !preferences.models.is_empty() => {
                client.admitted_models(bearer).await.ok()
            }
            _ => None,
        };
        let models = stream::iter(preferences.models.clone().into_iter().enumerate())
            .map(|(priority, (slug, filter))| {
                let bearer = &bearer;
                let admitted = admitted.as_ref();
                async move {
                    let priority = i32::try_from(priority.saturating_mul(2)).unwrap_or(i32::MAX);
                    let resolved = match &self.client {
                        Ok(client) => {
                            let token = match bearer {
                                Ok(token) => token.as_ref(),
                                // Public model metadata needs no key; only presets fail closed.
                                Err(_) => None,
                            };
                            client.resolve(&slug, token).await
                        }
                        Err(error) => Err(error.clone()),
                    };
                    match resolved {
                        Ok(resolved) => {
                            let info = normalize(&resolved, &filter, priority);
                            let mut rows = vec![info];
                            if let Some(admitted) = admitted
                                && !slug.as_str().contains(':')
                                && matches!(slug, ModelSlug::Model(_))
                                && let Ok(alias) = format!("{slug}:batch").parse::<ModelSlug>()
                                && admitted.contains(slug.as_str())
                                && admitted.contains(alias.as_str())
                                && let Ok(client) = &self.client
                                && let Ok(twin) = client
                                    .resolve(&alias, bearer.as_ref().ok().and_then(Option::as_ref))
                                    .await
                                && super::BatchRoute::pair(&resolved, &twin, &filter).is_ok()
                            {
                                let mut row = normalize(&twin, &filter, priority.saturating_add(1));
                                row.display_name = format!("{} (batch)", rows[0].display_name);
                                rows.push(row);
                            }
                            rows
                        }
                        Err(error) => {
                            let mut info = local(slug.as_str());
                            info.priority = priority;
                            info.description = Some(format!("Unavailable: {error}"));
                            vec![info]
                        }
                    }
                }
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
        let mut models = models.into_iter().flatten().collect::<Vec<_>>();
        models.sort_by_key(|info| info.priority);
        models
    }
}

impl ModelsManager for Catalog {
    fn ensure_initialized(&self, _factory: HttpClientFactory) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async move {
            self.models().await;
        })
    }

    fn get_default_model<'a>(
        &'a self,
        model: &'a Option<String>,
        _allow_fallback: bool,
        _strategy: RefreshStrategy,
        _factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'a, String> {
        Box::pin(async move {
            match model {
                Some(model) => model.clone(),
                None => self
                    .models()
                    .await
                    .into_iter()
                    .next()
                    .map(|info| info.slug)
                    .unwrap_or_default(),
            }
        })
    }

    fn model_catalog_with_origins(
        &self,
        _strategy: RefreshStrategy,
        _factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Vec<CatalogModel>> {
        Box::pin(async move {
            self.models()
                .await
                .into_iter()
                .map(|model| CatalogModel {
                    model: model.into(),
                    origin: ModelOrigin::Provider,
                })
                .collect()
        })
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        Box::pin(self.models())
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        Ok(self
            .snapshot
            .try_read()?
            .as_ref()
            .map_or_else(Vec::new, |snapshot| snapshot.models.clone()))
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        None
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        builtin_collaboration_mode_presets()
    }

    fn get_model_info<'a>(
        &'a self,
        model: &'a str,
        config: &'a ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, CatalogModel> {
        Box::pin(async move {
            let info = self
                .models()
                .await
                .into_iter()
                .find(|info| info.slug == model)
                .map_or_else(
                    || (local(model), ModelOrigin::Synthetic),
                    |info| (info, ModelOrigin::Provider),
                );
            CatalogModel {
                model: with_config_overrides(info.0, config).into(),
                origin: info.1,
            }
        })
    }

    fn refresh_if_new_etag(
        &self,
        _etag: String,
        _factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async move {
            self.models().await;
        })
    }
}

impl codex_models_manager::manager::AuthoritativeCatalog for Catalog {
    fn authoritative_catalog(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, codex_protocol::error::Result<ModelsResponse>> {
        Box::pin(async move { Ok(self.raw_model_catalog(strategy, factory).await) })
    }
}
