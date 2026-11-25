use super::TurnRequestProcessor;
use crate::config_manager::ConfigManager;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use crate::models::supported_models;
use codex_app_server_protocol::AccountProvider;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::OpenRouterEndpoint;
use codex_app_server_protocol::OpenRouterHost;
use codex_app_server_protocol::OpenRouterKey;
use codex_app_server_protocol::OpenRouterKeyAddParams;
use codex_app_server_protocol::OpenRouterKeyAddResponse;
use codex_app_server_protocol::OpenRouterKeyLimit;
use codex_app_server_protocol::OpenRouterKeyRemoveParams;
use codex_app_server_protocol::OpenRouterKeyRemoveResponse;
use codex_app_server_protocol::OpenRouterKeyState;
use codex_app_server_protocol::OpenRouterKeyStatus;
use codex_app_server_protocol::OpenRouterKeyStatusParams;
use codex_app_server_protocol::OpenRouterKeyStatusResponse;
use codex_app_server_protocol::OpenRouterModelEndpointsParams;
use codex_app_server_protocol::OpenRouterModelEndpointsResponse;
use codex_app_server_protocol::OpenRouterModelForgetParams;
use codex_app_server_protocol::OpenRouterModelForgetResponse;
use codex_app_server_protocol::OpenRouterModelRouteParams;
use codex_app_server_protocol::OpenRouterModelRouteResponse;
use codex_app_server_protocol::OpenRouterModelSaveParams;
use codex_app_server_protocol::OpenRouterModelSaveResponse;
use codex_app_server_protocol::OpenRouterSpend;
use codex_app_server_protocol::ThreadAccountSelectResponse;
use codex_config::OpenRouterConfig;
use codex_config::OpenRouterModelPreference;
use codex_config::read_openrouter_config;
use codex_core::CodexThread;
use codex_core::config::Config;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_goal_extension::QuotaError;
use codex_model_provider::openrouter::Bearer;
use codex_model_provider::openrouter::Client;
use codex_model_provider::openrouter::Error;
use codex_model_provider::openrouter::Price;
use codex_model_provider::openrouter::read_key_file;
use codex_model_provider::openrouter::shared_catalog;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::OPENROUTER_PROVIDER_ID;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::EndpointFilter;
use codex_protocol::EndpointSlug;
use codex_protocol::KeyName;
use codex_protocol::ModelSlug;
use codex_protocol::Usd;
use codex_protocol::goal_boxes::QuotaReading;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex;

const STATUS_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Default)]
pub(super) struct KeyStatusCache(Arc<Mutex<BTreeMap<KeyName, CachedKeyStatus>>>);

struct CachedKeyStatus {
    path: AbsolutePathBuf,
    base_url: String,
    at: Instant,
    state: OpenRouterKeyState,
}

impl KeyStatusCache {
    /// A key's state, fetched at most once per [`STATUS_TTL`].
    async fn state(&self, client: &Client, url: &str, key: &OpenRouterKey) -> OpenRouterKeyState {
        let cached = self
            .0
            .lock()
            .await
            .get(&key.name)
            .filter(|cached| {
                cached.path == key.path
                    && cached.base_url == url
                    && cached.at.elapsed() < STATUS_TTL
            })
            .map(|cached| cached.state.clone());
        if let Some(state) = cached {
            return state;
        }
        let state = fetch_state(client, &key.path)
            .await
            .unwrap_or_else(|error| OpenRouterKeyState::Failed { error });
        self.0.lock().await.insert(
            key.name.clone(),
            CachedKeyStatus {
                path: key.path.clone(),
                base_url: url.to_owned(),
                at: Instant::now(),
                state: state.clone(),
            },
        );
        state
    }
}

/// Reads `OpenRouter` key states through the server's shared status cache.
#[derive(Clone)]
pub(crate) struct KeyStatusReader {
    cache: KeyStatusCache,
    config_manager: ConfigManager,
}

impl KeyStatusReader {
    /// What an `OpenRouter` key can still spend: the lesser of its account's balance and what its
    /// own limit leaves. This is the quota a dollar quotabox watches.
    pub(crate) async fn usd_available(&self, name: &KeyName) -> Result<QuotaReading, QuotaError> {
        let unreadable = |error: JSONRPCErrorError| QuotaError::Unreadable(error.message);
        let config = self
            .config_manager
            .load_non_project_config()
            .await
            .map_err(|error| QuotaError::Unreadable(error.to_string()))?;
        let key =
            key(&preferences(&config).await.map_err(unreadable)?, name).map_err(unreadable)?;
        let client = client(&config).map_err(unreadable)?;
        let url = base_url(&config).map_err(unreadable)?;
        if openrouter_liability(&config, url, &key)
            .await
            .map_err(QuotaError::Unreadable)?
            > 0
        {
            // A dollar quota watches guaranteed available funds, not an optimistic balance.
            return Ok(QuotaReading::UsdAvailable {
                usd: Usd::default(),
            });
        }
        match self.cache.state(&client, url, &key).await {
            OpenRouterKeyState::Read { balance, limit, .. } => balance
                .into_iter()
                .chain(limit.map(|limit| limit.remaining))
                .min()
                .map(|usd| QuotaReading::UsdAvailable { usd })
                .ok_or_else(|| {
                    QuotaError::Unsupported(format!(
                        "OpenRouter key {name} reports neither a balance nor a limit."
                    ))
                }),
            OpenRouterKeyState::Failed { error } => Err(QuotaError::Unreadable(error)),
        }
    }
}

async fn openrouter_liability(
    config: &Config,
    url: &str,
    key: &OpenRouterKey,
) -> Result<u32, String> {
    let fingerprint = read_key_file(key.path.as_path())
        .await
        .map_err(|e| e.to_string())?
        .batch_fingerprint();
    let home = config.codex_home.clone();
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || {
        codex_core::outstanding_openrouter_batches(&home, &url, &fingerprint)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

fn base_url(config: &Config) -> Result<&str, JSONRPCErrorError> {
    config
        .model_providers
        .get(OPENROUTER_PROVIDER_ID)
        .map(|info| {
            info.base_url
                .as_deref()
                .unwrap_or("https://openrouter.ai/api/v1")
        })
        .ok_or_else(|| internal_error("OpenRouter provider is unavailable"))
}

pub(in crate::request_processors) fn catalog(
    config: &Config,
) -> Result<SharedModelsManager, JSONRPCErrorError> {
    Ok(shared_catalog(
        &config.codex_home,
        base_url(config)?,
        config.http_client_factory(),
    ))
}

pub(in crate::request_processors) fn catalog_for_list(
    config: &Config,
) -> Result<Option<SharedModelsManager>, JSONRPCErrorError> {
    match config.model_provider.adapter {
        ModelProviderAdapter::OpenRouter => catalog(config).map(Some),
        ModelProviderAdapter::ClaudeSubscription | ModelProviderAdapter::OpenaiCompatible => {
            Ok(None)
        }
    }
}

pub(in crate::request_processors) async fn offer_provider(
    config: &Config,
    options: &mut Vec<codex_app_server_protocol::ModelProviderOption>,
) -> Result<(), JSONRPCErrorError> {
    options.retain(|option| option.id != OPENROUTER_PROVIDER_ID);
    if !preferences(config).await?.key_files.is_empty() {
        let info = config
            .model_providers
            .get(OPENROUTER_PROVIDER_ID)
            .ok_or_else(|| internal_error("OpenRouter provider is unavailable"))?;
        options.push(codex_app_server_protocol::ModelProviderOption {
            id: OPENROUTER_PROVIDER_ID.to_owned(),
            name: info.name.clone(),
        });
    }
    Ok(())
}

fn client(config: &Config) -> Result<Client, JSONRPCErrorError> {
    Client::new(base_url(config)?, config.http_client_factory())
        .map_err(|error| invalid_request(error.to_string()))
}

async fn preferences(config: &Config) -> Result<OpenRouterConfig, JSONRPCErrorError> {
    read_openrouter_config(&config.codex_home)
        .await
        .map_err(|_| internal_error("Cannot read OpenRouter preferences"))
}

fn key(preferences: &OpenRouterConfig, name: &KeyName) -> Result<OpenRouterKey, JSONRPCErrorError> {
    let path = preferences
        .key_files
        .get(name)
        .ok_or_else(|| invalid_request(format!("OpenRouter key {name} is not configured")))?;
    Ok(OpenRouterKey {
        name: name.clone(),
        path: path.clone(),
        is_default: preferences.default_key() == Some(name),
    })
}

fn path(section: &str, name: &str) -> Vec<String> {
    vec![
        "wrangler".into(),
        "openrouter".into(),
        section.into(),
        name.into(),
    ]
}

fn default_key(name: &KeyName) -> ConfigEdit {
    ConfigEdit::SetPath {
        segments: vec!["wrangler".into(), "openrouter".into(), "default_key".into()],
        value: toml_edit::value(name.as_str()),
    }
}

async fn apply(
    config: &Config,
    edits: impl IntoIterator<Item = ConfigEdit>,
) -> Result<(), JSONRPCErrorError> {
    ConfigEditsBuilder::new(&config.codex_home)
        .with_edits(edits)
        .apply()
        .await
        .map_err(|_| internal_error("Cannot save OpenRouter preferences"))
}

fn filter_value(filter: &EndpointFilter) -> toml_edit::Item {
    let mut table = toml_edit::InlineTable::new();
    let (field, set) = match filter {
        EndpointFilter::All => return toml_edit::Item::Value(table.into()),
        EndpointFilter::Only(set) => ("only", set),
        EndpointFilter::Ignore(set) => ("ignore", set),
    };
    let array = set
        .slugs()
        .iter()
        .map(EndpointSlug::as_str)
        .collect::<toml_edit::Array>();
    table.insert(field, array.into());
    toml_edit::Item::Value(table.into())
}

enum ModelEdit<'a> {
    Save,
    Forget,
    Route(&'a EndpointFilter),
}

async fn edit_model(
    config: &Config,
    slug: &ModelSlug,
    edit: ModelEdit<'_>,
) -> Result<(), JSONRPCErrorError> {
    use OpenRouterModelPreference::Forgotten;
    use OpenRouterModelPreference::Saved;
    use OpenRouterModelPreference::Unlisted;

    if matches!(edit, ModelEdit::Route(_)) && matches!(slug, ModelSlug::Preset(_)) {
        return Err(invalid_request(
            "OpenRouter presets own their endpoint routing",
        ));
    }
    let _lock = codex_config::lock_account_preferences(&config.codex_home)
        .await
        .map_err(|_| internal_error("Cannot lock OpenRouter preferences"))?;
    let prefs = preferences(config).await?;
    let clear = |section| ConfigEdit::ClearPath {
        segments: path(section, slug.as_str()),
    };
    let set = |section, filter| ConfigEdit::SetPath {
        segments: path(section, slug.as_str()),
        value: filter_value(filter),
    };

    let edits = match (edit, prefs.model_preference(slug)) {
        (ModelEdit::Save, Saved(_)) | (ModelEdit::Forget, Forgotten(_) | Unlisted) => return Ok(()),
        (ModelEdit::Save, Unlisted) => vec![set("models", &EndpointFilter::All)],
        (ModelEdit::Save, Forgotten(filter)) => {
            vec![set("models", filter), clear("forgotten_models")]
        }
        // Forget removes the picker entry, not the permission boundary.
        (ModelEdit::Forget, Saved(EndpointFilter::All)) => vec![clear("models")],
        (ModelEdit::Forget, Saved(filter)) => {
            vec![clear("models"), set("forgotten_models", filter)]
        }
        (ModelEdit::Route(filter), Saved(_)) => vec![set("models", filter)],
        (ModelEdit::Route(_), Forgotten(_) | Unlisted) => {
            return Err(invalid_request(
                "Save the OpenRouter model before choosing its endpoints",
            ));
        }
    };
    apply(config, edits).await
}

async fn default_bearer(
    preferences: &OpenRouterConfig,
) -> Result<Option<Bearer>, JSONRPCErrorError> {
    match preferences
        .default_key()
        .and_then(|name| preferences.key_files.get(name))
    {
        Some(path) => read_key_file(path.as_path())
            .await
            .map(Some)
            .map_err(|error| invalid_request(error.to_string())),
        None => Ok(None),
    }
}

async fn fetch_state(
    client: &Client,
    path: &AbsolutePathBuf,
) -> Result<OpenRouterKeyState, String> {
    let bearer = read_key_file(path.as_path())
        .await
        .map_err(|error| error.to_string())?;
    let (key, credits) = tokio::join!(client.key(&bearer), client.credits(&bearer));
    let key = key.map_err(|error| error.to_string())?;
    let expires_at = key
        .expires_at
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| Error::InvalidResponse.to_string())?
        .map(|time| time.timestamp());
    let limit = match (key.limit, key.limit_remaining) {
        (None, _) => None,
        (Some(limit), Some(remaining)) => Some(OpenRouterKeyLimit {
            limit,
            remaining,
            reset: key.limit_reset,
        }),
        (Some(_), None) => return Err(Error::InvalidResponse.to_string()),
    };
    Ok(OpenRouterKeyState::Read {
        label: key.label,
        balance: credits.ok().map(|credits| {
            Usd::from_nanodollars(
                credits
                    .total_credits
                    .nanodollars()
                    .saturating_sub(credits.total_usage.nanodollars()),
            )
        }),
        limit,
        spend: OpenRouterSpend {
            daily: key.usage_daily,
            weekly: key.usage_weekly,
            monthly: key.usage_monthly,
        },
        expires_at,
        captured_at: chrono::Utc::now().timestamp(),
    })
}

impl TurnRequestProcessor {
    pub(crate) fn openrouter_key_reader(&self) -> KeyStatusReader {
        KeyStatusReader {
            cache: self.openrouter_key_cache.clone(),
            config_manager: self.config_manager.clone(),
        }
    }

    async fn openrouter_config(&self) -> Result<Arc<Config>, JSONRPCErrorError> {
        self.config_manager
            .load_non_project_config()
            .await
            .map(Arc::new)
            .map_err(|error| internal_error(error.to_string()))
    }

    pub(super) async fn openrouter_key_statuses(
        &self,
        config: &Config,
        name: Option<&KeyName>,
    ) -> Result<Vec<OpenRouterKeyStatus>, JSONRPCErrorError> {
        let preferences = preferences(config).await?;
        let keys = match name {
            Some(name) => vec![key(&preferences, name)?],
            None => preferences
                .key_files
                .keys()
                .map(|name| key(&preferences, name))
                .collect::<Result<Vec<_>, _>>()?,
        };
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let client = client(config)?;
        let url = base_url(config)?;
        self.openrouter_key_cache
            .0
            .lock()
            .await
            .retain(|name, _| preferences.key_files.contains_key(name));
        let mut statuses = Vec::with_capacity(keys.len());
        for key in keys {
            let state = self.openrouter_key_cache.state(&client, url, &key).await;
            let outstanding_batches = openrouter_liability(config, url, &key).await;
            let (state, outstanding_batches) = match outstanding_batches {
                Ok(count) => (state, count),
                Err(error) => (OpenRouterKeyState::Failed { error }, 0),
            };
            statuses.push(OpenRouterKeyStatus {
                key,
                state,
                outstanding_batches,
            });
        }
        Ok(statuses)
    }

    pub(crate) async fn openrouter_key_add(
        &self,
        params: OpenRouterKeyAddParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let config = self.openrouter_config().await?;
        if preferences(&config)
            .await?
            .key_files
            .contains_key(&params.name)
        {
            return Err(invalid_request(format!(
                "OpenRouter key {} already exists",
                params.name
            )));
        }
        let metadata = tokio::fs::metadata(params.path.as_path())
            .await
            .map_err(|_| invalid_request("Cannot read the OpenRouter key file"))?;
        if !metadata.is_file() {
            return Err(invalid_request(
                "OpenRouter key path must name a regular file",
            ));
        }
        let bearer = read_key_file(params.path.as_path())
            .await
            .map_err(|error| invalid_request(error.to_string()))?;
        client(&config)?
            .key(&bearer)
            .await
            .map_err(|error| invalid_request(error.to_string()))?;
        let _lock = codex_config::lock_account_preferences(&config.codex_home)
            .await
            .map_err(|_| internal_error("Cannot lock OpenRouter preferences"))?;
        let mut preferences = preferences(&config).await?;
        if preferences.key_files.contains_key(&params.name) {
            return Err(invalid_request(format!(
                "OpenRouter key {} already exists",
                params.name
            )));
        }
        let mut edits = vec![ConfigEdit::SetPath {
            segments: path("key_files", params.name.as_str()),
            value: toml_edit::value(params.path.to_string_lossy().as_ref()),
        }];
        if preferences.key_files.is_empty() {
            edits.push(default_key(&params.name));
            preferences.default_key = Some(params.name.clone());
        }
        preferences
            .key_files
            .insert(params.name.clone(), params.path);
        apply(&config, edits).await?;
        self.openrouter_key_cache
            .0
            .lock()
            .await
            .remove(&params.name);
        Ok(Some(
            OpenRouterKeyAddResponse {
                key: key(&preferences, &params.name)?,
            }
            .into(),
        ))
    }

    pub(crate) async fn openrouter_key_remove(
        &self,
        params: OpenRouterKeyRemoveParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let config = self.openrouter_config().await?;
        let _lock = codex_config::lock_account_preferences(&config.codex_home)
            .await
            .map_err(|_| internal_error("Cannot lock OpenRouter preferences"))?;
        let preferences = preferences(&config).await?;
        let mut edits = vec![ConfigEdit::ClearPath {
            segments: path("key_files", params.name.as_str()),
        }];
        if preferences.default_key.as_ref() == Some(&params.name) {
            edits.push(ConfigEdit::ClearPath {
                segments: vec!["wrangler".into(), "openrouter".into(), "default_key".into()],
            });
        }
        apply(&config, edits).await?;
        self.openrouter_key_cache
            .0
            .lock()
            .await
            .remove(&params.name);
        Ok(Some(OpenRouterKeyRemoveResponse {}.into()))
    }

    pub(crate) async fn openrouter_key_status(
        &self,
        params: OpenRouterKeyStatusParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let config = self.openrouter_config().await?;
        Ok(Some(
            OpenRouterKeyStatusResponse {
                keys: self
                    .openrouter_key_statuses(&config, params.name.as_ref())
                    .await?,
            }
            .into(),
        ))
    }

    pub(crate) async fn openrouter_model_save(
        &self,
        params: OpenRouterModelSaveParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let mut params = params;
        if let Some(base) = params.slug.as_str().strip_suffix(":batch") {
            params.slug = base.parse().map_err(invalid_request)?;
        }
        let config = self.openrouter_config().await?;
        let prefs = preferences(&config).await?;
        let bearer = default_bearer(&prefs).await?;
        if matches!(params.slug, ModelSlug::Preset(_)) && bearer.is_none() {
            return Err(invalid_request("Add an OpenRouter key first"));
        }
        let resolved = client(&config)?
            .resolve(&params.slug, bearer.as_ref())
            .await
            .map_err(|error| {
                invalid_request(match error {
                    Error::Http(status) if matches!(status.as_u16(), 400 | 404) => {
                        format!("OpenRouter has no model {}", params.slug)
                    }
                    error => error.to_string(),
                })
            })?;
        if !resolved.model.supports_tools() {
            return Err(invalid_request(format!(
                "{} does not support tool calling",
                params.slug
            )));
        }
        edit_model(&config, &params.slug, ModelEdit::Save).await?;
        let presets = catalog(&config)?
            .list_models(
                RefreshStrategy::OnlineIfUncached,
                config.http_client_factory(),
            )
            .await;
        let model = supported_models(presets, true)
            .into_iter()
            .find(|model| model.model == params.slug.as_str())
            .ok_or_else(|| internal_error("Saved OpenRouter model is missing from its catalog"))?;
        Ok(Some(OpenRouterModelSaveResponse { model }.into()))
    }

    pub(crate) async fn openrouter_model_forget(
        &self,
        params: OpenRouterModelForgetParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let config = self.openrouter_config().await?;
        edit_model(&config, &params.slug, ModelEdit::Forget).await?;
        Ok(Some(OpenRouterModelForgetResponse {}.into()))
    }

    pub(crate) async fn openrouter_model_endpoints(
        &self,
        params: OpenRouterModelEndpointsParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        if matches!(params.slug, ModelSlug::Preset(_)) {
            return Err(invalid_request(
                "OpenRouter presets own their endpoint routing",
            ));
        }
        let config = self.openrouter_config().await?;
        let preferences = preferences(&config).await?;
        let bearer = default_bearer(&preferences).await?;
        let endpoints = client(&config)?
            .endpoints(&params.slug, bearer.as_ref())
            .await
            .map_err(|error| invalid_request(error.to_string()))?;
        let mut hosts = BTreeMap::<EndpointSlug, OpenRouterHost>::new();
        for endpoint in endpoints {
            let tag = endpoint.tag.as_str();
            let host = tag.split_once('/').map_or(tag, |(host, _)| host);
            let slug = host
                .parse()
                .map_err(|_| internal_error("OpenRouter endpoint has an invalid host"))?;
            hosts
                .entry(slug)
                .or_insert_with_key(|slug| OpenRouterHost {
                    slug: slug.clone(),
                    name: endpoint.host_name,
                    endpoints: Vec::new(),
                })
                .endpoints
                .push(OpenRouterEndpoint {
                    slug: endpoint.tag,
                    quantization: endpoint.quantization,
                    service_tier: endpoint.service_tier,
                    context_length: endpoint.context_length,
                    prompt_per_mtok: endpoint.pricing.prompt.map(Price::per_mtok),
                    completion_per_mtok: endpoint.pricing.completion.map(Price::per_mtok),
                    uptime_percent: endpoint.uptime_percent,
                });
        }
        Ok(Some(
            OpenRouterModelEndpointsResponse {
                hosts: hosts.into_values().collect(),
                filter: preferences.model_preference(&params.slug).endpoint_filter(),
            }
            .into(),
        ))
    }

    pub(crate) async fn openrouter_model_route(
        &self,
        params: OpenRouterModelRouteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let config = self.openrouter_config().await?;
        edit_model(&config, &params.slug, ModelEdit::Route(&params.filter)).await?;
        Ok(Some(OpenRouterModelRouteResponse {}.into()))
    }

    pub(super) async fn openrouter_key_select(
        &self,
        config: &Config,
        thread: &CodexThread,
        account_id: String,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let name: KeyName = account_id.parse().map_err(invalid_request)?;
        let _lock = codex_config::lock_account_preferences(&config.codex_home)
            .await
            .map_err(|_| internal_error("Cannot lock OpenRouter preferences"))?;
        let preferences = preferences(config).await?;
        key(&preferences, &name)?;
        let is_openrouter = matches!(
            thread.config_snapshot().await.model_provider_adapter,
            ModelProviderAdapter::OpenRouter
        );
        if is_openrouter {
            Box::pin(thread.select_openrouter_key(&name))
                .await
                .map_err(|error| invalid_request(error.to_string()))?;
        }
        let default_saved = apply(config, [default_key(&name)])
            .await
            .map_err(|error| {
                tracing::warn!(code = error.code, "Cannot save default OpenRouter key");
            })
            .is_ok();
        Ok(Some(
            ThreadAccountSelectResponse {
                account_id,
                provider: AccountProvider::OpenRouter,
                conversation_changed: is_openrouter,
                pending: is_openrouter && thread.pending_account_id().is_some(),
                default_saved,
            }
            .into(),
        ))
    }
}
