use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::RouteAwareClientPool;
use codex_protocol::EndpointSlug;
use codex_protocol::ModelSlug;
use codex_protocol::Usd;
use codex_protocol::openai_models::ReasoningEffort;
use http::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

const MAX_BODY: usize = 4 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidBaseUrl,
    Transport,
    Timeout,
    Http(StatusCode),
    TooLarge,
    InvalidResponse,
    PresetNeedsKey,
    BatchUnavailable,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBaseUrl => f.write_str("invalid OpenRouter base URL"),
            Self::Transport => f.write_str("OpenRouter connection failed"),
            Self::Timeout => f.write_str("OpenRouter request timed out"),
            Self::Http(status) => write!(f, "OpenRouter returned HTTP {}", status.as_u16()),
            Self::TooLarge => f.write_str("OpenRouter response exceeds its size limit"),
            Self::InvalidResponse => f.write_str("invalid OpenRouter response"),
            Self::PresetNeedsKey => f.write_str("Choose an OpenRouter key with /account"),
            Self::BatchUnavailable => f.write_str(
                "No compatible OpenRouter batch route under this model, key and endpoint selection",
            ),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Deserialize)]
pub struct Envelope<T> {
    pub data: T,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    pub id: ModelSlug,
    pub name: String,
    pub canonical_slug: Option<String>,
    #[serde(default)]
    pub endpoints: Vec<String>,
    pub context_length: i64,
    pub architecture: Architecture,
    pub supported_parameters: Vec<String>,
    pub pricing: Pricing,
    pub expiration_date: Option<String>,
    pub reasoning: Option<Reasoning>,
}

impl Model {
    pub fn supports_tools(&self) -> bool {
        self.supported_parameters
            .iter()
            .any(|value| value == "tools")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Architecture {
    pub input_modalities: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Reasoning {
    pub supported_efforts: Vec<ReasoningEffort>,
    pub default_effort: Option<ReasoningEffort>,
    pub mandatory: bool,
}

// Preserve per-token fractions until converting to per-million prices; rounding
// a fraction of a nanodollar first would corrupt cheap models' displayed prices.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(try_from = "String")]
pub struct Price(Usd);

impl TryFrom<String> for Price {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let dollars = value.parse::<f64>().map_err(|_| "invalid token price")?;
        if !dollars.is_finite() || dollars < 0.0 {
            return Err("token price is outside the supported range");
        }
        Usd::try_from(dollars * 1_000_000.0)
            .map(Self)
            .map_err(|_| "token price is outside the supported range")
    }
}

impl Price {
    pub fn per_mtok(self) -> Usd {
        self.0
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Pricing {
    pub prompt: Option<Price>,
    pub completion: Option<Price>,
    pub input_cache_read: Option<Price>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Endpoint {
    pub tag: EndpointSlug,
    #[serde(default)]
    pub supported_parameters: Vec<String>,
    #[serde(rename = "provider_name")]
    pub host_name: String,
    pub quantization: Option<String>,
    pub service_tier: Option<String>,
    pub context_length: i64,
    pub pricing: Pricing,
    #[serde(rename = "uptime_last_30m")]
    pub uptime_percent: Option<f64>,
    pub status: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Endpoints {
    pub endpoints: Vec<Endpoint>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Preset {
    pub designated_version: PresetVersion,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PresetVersion {
    pub config: PresetConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PresetConfig {
    pub model: ModelSlug,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Key {
    pub label: String,
    pub limit: Option<Usd>,
    pub limit_remaining: Option<Usd>,
    pub limit_reset: Option<String>,
    pub usage_daily: Usd,
    pub usage_weekly: Usd,
    pub usage_monthly: Usd,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Credits {
    pub total_credits: Usd,
    pub total_usage: Usd,
}

#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub slug: ModelSlug,
    pub model: Model,
    pub endpoints: Vec<Endpoint>,
}

// Neither credentials nor server-supplied diagnostics are retained by the client.
#[derive(Clone)]
pub struct Client {
    base_url: Url,
    pool: Arc<RouteAwareClientPool>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenRouterClient").finish_non_exhaustive()
    }
}

impl Client {
    pub(super) fn admits_global_batch(&self) -> bool {
        match self.base_url.host() {
            Some(url::Host::Domain("openrouter.ai")) => {
                self.base_url.scheme() == "https"
                    && self.base_url.port_or_known_default() == Some(443)
            }
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        }
    }
    pub fn new(base_url: &str, factory: HttpClientFactory) -> Result<Self, Error> {
        let mut base_url = Url::parse(base_url).map_err(|_| Error::InvalidBaseUrl)?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(Error::InvalidBaseUrl);
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self {
            base_url,
            pool: Arc::new(
                RouteAwareClientPool::new_without_redirects_or_request_logging(
                    factory,
                    ClientRouteClass::Api,
                ),
            ),
        })
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        bearer: Option<&super::Bearer>,
    ) -> Result<T, Error> {
        self.exchange::<Envelope<T>>(self.request(http::Method::GET, path)?, bearer, MAX_BODY)
            .await
            .map(|envelope| envelope.data)
    }

    pub(super) fn request(
        &self,
        method: http::Method,
        path: &str,
    ) -> Result<codex_http_client::RequestBuilder, Error> {
        let url = self
            .base_url
            .join(path)
            .map_err(|_| Error::InvalidBaseUrl)?;
        Ok(self.pool.request(method, url))
    }

    pub(super) async fn exchange<T: DeserializeOwned>(
        &self,
        request: codex_http_client::RequestBuilder,
        bearer: Option<&super::Bearer>,
        limit: usize,
    ) -> Result<T, Error> {
        tokio::time::timeout(DEADLINE, async {
            let request = match bearer {
                Some(bearer) => request.header(http::header::AUTHORIZATION, bearer.expose()),
                None => request,
            };
            let mut response = request.send().await.map_err(|_| Error::Transport)?;
            let status = response.status();
            if !status.is_success() {
                return Err(Error::Http(status));
            }
            if response
                .content_length()
                .is_some_and(|size| size > limit as u64)
            {
                return Err(Error::TooLarge);
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
                if chunk.len() > limit.saturating_sub(body.len()) {
                    return Err(Error::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&body).map_err(|_| Error::InvalidResponse)
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    pub async fn model(
        &self,
        slug: &ModelSlug,
        bearer: Option<&super::Bearer>,
    ) -> Result<Model, Error> {
        self.get(&format!("model/{slug}"), bearer).await
    }

    pub async fn endpoints(
        &self,
        slug: &ModelSlug,
        bearer: Option<&super::Bearer>,
    ) -> Result<Vec<Endpoint>, Error> {
        self.get::<Endpoints>(&format!("models/{slug}/endpoints"), bearer)
            .await
            .map(|data| data.endpoints)
    }

    pub async fn preset(&self, slug: &ModelSlug, bearer: &super::Bearer) -> Result<Preset, Error> {
        let name = slug
            .as_str()
            .strip_prefix("@preset/")
            .ok_or(Error::InvalidResponse)?;
        self.get(&format!("presets/{name}"), Some(bearer)).await
    }

    pub async fn resolve(
        &self,
        slug: &ModelSlug,
        bearer: Option<&super::Bearer>,
    ) -> Result<ResolvedModel, Error> {
        let target = match slug {
            ModelSlug::Model(_) => slug.clone(),
            ModelSlug::Preset(_) => {
                self.preset(slug, bearer.ok_or(Error::PresetNeedsKey)?)
                    .await?
                    .designated_version
                    .config
                    .model
            }
        };
        if matches!(target, ModelSlug::Preset(_)) {
            return Err(Error::InvalidResponse);
        }
        let (model, endpoints) =
            tokio::try_join!(self.model(&target, bearer), self.endpoints(&target, bearer))?;
        Ok(ResolvedModel {
            slug: slug.clone(),
            model,
            endpoints,
        })
    }

    pub async fn key(&self, bearer: &super::Bearer) -> Result<Key, Error> {
        self.get("key", Some(bearer)).await
    }

    pub async fn credits(&self, bearer: &super::Bearer) -> Result<Credits, Error> {
        self.get("credits", Some(bearer)).await
    }
}
