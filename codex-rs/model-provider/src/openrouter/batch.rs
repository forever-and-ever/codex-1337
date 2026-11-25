//! `OpenRouter` batch admission and one-row jobs. A lost POST response can hide a charge.

use super::Bearer;
use super::Client;
use super::Envelope;
use super::Error;
use super::ResolvedModel;
use codex_api::ResponsesApiRequest;
use codex_protocol::EndpointFilter;
use codex_protocol::ModelSlug;
use codex_protocol::Usd;
use http::Method;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeSet;

const RESULTS_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchRoute {
    model: ModelSlug,
    canonical_slug: String,
    hosts: BTreeSet<String>,
    live_images: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LiveReason {
    CompactionNeedsInlineImage,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BatchDispatch {
    Batch,
    NecessaryLive(LiveReason),
}

impl BatchRoute {
    pub fn model(&self) -> &ModelSlug {
        &self.model
    }

    pub(super) fn pair(
        live: &ResolvedModel,
        batch: &ResolvedModel,
        filter: &EndpointFilter,
    ) -> Result<Self, Error> {
        let canonical_slug = live
            .model
            .canonical_slug
            .as_ref()
            .filter(|id| !id.is_empty())
            .ok_or(Error::BatchUnavailable)?;
        if live.slug != live.model.id
            || batch.slug != batch.model.id
            || batch.slug.as_str() != format!("{}:batch", live.slug)
            || batch.model.canonical_slug.as_ref() != Some(canonical_slug)
            || !live
                .model
                .endpoints
                .iter()
                .any(|path| path == "/api/v1/responses")
            || !batch
                .model
                .endpoints
                .iter()
                .any(|path| path == "/api/v1/batches")
            || !live.model.supports_tools()
            || !batch.model.supports_tools()
        {
            return Err(Error::BatchUnavailable);
        }
        let hosts = batch
            .endpoints
            .iter()
            .filter(|ep| ep.status == 0 && ep.supported_parameters.iter().any(|p| p == "tools"))
            .filter_map(|ep| ep.tag.as_str().split('/').next())
            // These hosts accept batch Chat Completions, not batch Responses.
            .filter(|host| !matches!(*host, "deepinfra" | "alibaba"))
            // The batch API admits hosts, not individual regions or variants. A host must
            // be wholly permitted; currently listed endpoints do not close its namespace.
            .filter(|host| {
                filter.admits(host)
                    && match filter {
                        EndpointFilter::Ignore(set) => !set
                            .slugs()
                            .iter()
                            .any(|tag| tag.as_str().split('/').next() == Some(*host)),
                        EndpointFilter::All | EndpointFilter::Only(_) => true,
                    }
            })
            .filter(|host| {
                live.endpoints.iter().any(|ep| {
                    ep.status == 0
                        && ep.tag.as_str().split('/').next() == Some(*host)
                        && ep.supported_parameters.iter().any(|p| p == "tools")
                })
            })
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        if hosts.is_empty() {
            return Err(Error::BatchUnavailable);
        }
        Ok(Self {
            model: live.slug.clone(),
            canonical_slug: canonical_slug.clone(),
            hosts,
            live_images: live
                .model
                .architecture
                .input_modalities
                .iter()
                .any(|kind| kind == "image"),
        })
    }

    pub fn dispatch(
        &self,
        request: &ResponsesApiRequest,
        compaction: bool,
    ) -> Result<BatchDispatch, Error> {
        use codex_protocol::models::ContentItem;
        use codex_protocol::models::FunctionCallOutputBody;
        use codex_protocol::models::FunctionCallOutputContentItem;
        use codex_protocol::models::ImageReference;
        use codex_protocol::models::ResponseItem;
        // Inline images need a separate account allowlist that model discovery
        // does not attest. A native file ID is not a public batch input either.
        let mut dispatch = BatchDispatch::Batch;
        let mut image = |reference: &ImageReference| match reference {
            ImageReference::Inline { image_url } if image_url.starts_with("https://") => Ok(()),
            ImageReference::Inline { image_url }
                if compaction
                    && self.live_images
                    && (image_url.starts_with("data:image/png;base64,")
                        || image_url.starts_with("data:image/jpeg;base64,"))
                    && self
                        .hosts
                        .iter()
                        .all(|host| matches!(host.as_str(), "together" | "fireworks")) =>
            {
                // Both live vision APIs accept inline PNG/JPEG. Neither batch
                // host admits inline images, even on an allowlisted account.
                dispatch = BatchDispatch::NecessaryLive(LiveReason::CompactionNeedsInlineImage);
                Ok(())
            }
            _ => Err(Error::BatchUnavailable),
        };
        for item in &request.input {
            match item {
                ResponseItem::Message { content, .. } => {
                    for part in content {
                        match part {
                            ContentItem::InputImage {
                                image: reference, ..
                            } => image(reference)?,
                            ContentItem::InputAudio { .. } => return Err(Error::BatchUnavailable),
                            _ => {}
                        }
                    }
                }
                ResponseItem::FunctionCallOutput { output, .. }
                | ResponseItem::CustomToolCallOutput { output, .. } => {
                    if let FunctionCallOutputBody::ContentItems(parts) = &output.body {
                        for part in parts {
                            match part {
                                FunctionCallOutputContentItem::InputImage {
                                    image: reference,
                                    ..
                                } => image(reference)?,
                                FunctionCallOutputContentItem::InputAudio { .. } => {
                                    return Err(Error::BatchUnavailable);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(dispatch)
    }

    /// The serialized order is a wire contract: `OpenRouter` reads routing fields before requests.
    pub fn encode(
        &self,
        custom_id: &str,
        mut request: ResponsesApiRequest,
        dispatch: BatchDispatch,
    ) -> Result<Vec<u8>, Error> {
        #[derive(Serialize)]
        struct Provider<'a> {
            only: &'a BTreeSet<String>,
            allow_fallbacks: bool,
        }
        #[derive(Serialize)]
        struct Row<'a> {
            custom_id: &'a str,
            body: ResponsesApiRequest,
        }
        #[derive(Serialize)]
        struct Submission<'a> {
            endpoint: &'static str,
            model: &'a ModelSlug,
            provider: Provider<'a>,
            requests: [Row<'a>; 1],
        }
        if self.model.as_str() != request.model || request.store || request.service_tier.is_some() {
            return Err(Error::BatchUnavailable);
        }
        request.openrouter = None;
        request.stream = false;
        request.stream_options = None;
        if matches!(dispatch, BatchDispatch::NecessaryLive(_)) {
            let mut body = serde_json::to_value(request).map_err(|_| Error::InvalidResponse)?;
            body["provider"] = serde_json::json!({"only":self.hosts,"allow_fallbacks":false});
            return serde_json::to_vec(&body).map_err(|_| Error::InvalidResponse);
        }
        serde_json::to_vec(&Submission {
            endpoint: "/v1/responses",
            model: &self.model,
            provider: Provider {
                only: &self.hosts,
                allow_fallbacks: false,
            },
            requests: [Row {
                custom_id,
                body: request,
            }],
        })
        .map_err(|_| Error::InvalidResponse)
    }
}

/// A remote identifier is untrusted input, not a relative URL.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub struct BatchId(String);

impl TryFrom<String> for BatchId {
    type Error = &'static str;
    fn try_from(id: String) -> Result<Self, Self::Error> {
        if id.len() > 160
            || !(id.starts_with("batch_") || id.starts_with("batch-"))
            || id.len() == 6
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        {
            return Err("invalid batch id");
        }
        Ok(Self(id))
    }
}

impl From<BatchId> for String {
    fn from(id: BatchId) -> Self {
        id.0
    }
}

// Decode the wire object directly: Serde's internally tagged/flattened buffering
// turns arbitrary-precision JSON decimals into maps, losing the invoice.
#[derive(Debug, Deserialize)]
struct Job {
    id: BatchId,
    status: JobStatus,
    results: Option<Vec<Row>>,
    usage: Option<BatchUsage>,
    error: Option<RowError>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JobStatus {
    Validating,
    InProgress,
    Finalizing,
    Completed,
    Failed,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchUsage {
    #[serde(alias = "input_tokens")]
    pub prompt_tokens: i64,
    #[serde(alias = "output_tokens")]
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cost: Option<Usd>,
}

#[derive(Debug, Deserialize)]
struct Row {
    custom_id: String,
    response: Option<RowResponse>,
    error: Option<RowError>,
}

#[derive(Debug, Deserialize)]
struct RowResponse {
    status_code: u16,
    body: Value,
}

#[derive(Debug, Deserialize)]
struct RowError {
    message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settlement {
    pub usage: Option<BatchUsage>,
    pub response: Result<Value, String>,
}

impl Client {
    pub async fn necessary_live_response(
        &self,
        bearer: &Bearer,
        wire: Vec<u8>,
    ) -> Result<Settlement, Error> {
        let body: Value = self
            .exchange(
                self.request(Method::POST, "responses")?
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(wire),
                Some(bearer),
                RESULTS_LIMIT,
            )
            .await?;
        let usage = body
            .get("usage")
            .cloned()
            .filter(|v| !v.is_null())
            .map(serde_json::from_value::<BatchUsage>)
            .transpose()
            .map_err(|_| Error::InvalidResponse)?;
        Ok(Settlement {
            usage,
            response: Ok(body),
        })
    }
    pub(super) async fn admitted_models(&self, bearer: &Bearer) -> Result<BTreeSet<String>, Error> {
        #[derive(Deserialize)]
        struct UserModel {
            id: String,
        }
        if !self.admits_global_batch() {
            return Err(Error::BatchUnavailable);
        }
        self.exchange::<Envelope<Vec<UserModel>>>(
            self.request(Method::GET, "models/user")?,
            Some(bearer),
            16 * 1024 * 1024,
        )
        .await
        .map(|models| models.data.into_iter().map(|m| m.id).collect())
    }

    pub async fn batch_route(
        &self,
        slug: &ModelSlug,
        filter: &EndpointFilter,
        bearer: &Bearer,
    ) -> Result<BatchRoute, Error> {
        if !matches!(slug, ModelSlug::Model(name) if !name.contains(':') && !name.starts_with('~'))
        {
            return Err(Error::BatchUnavailable);
        }
        let alias: ModelSlug = format!("{slug}:batch")
            .parse()
            .map_err(|_| Error::BatchUnavailable)?;
        // The public catalog is not evidence that this key's privacy settings and
        // guardrails allow a model. Check its user-filtered catalog before sending input.
        let admitted = self.admitted_models(bearer).await?;
        if !admitted.contains(slug.as_str()) || !admitted.contains(alias.as_str()) {
            return Err(Error::BatchUnavailable);
        }
        let (live, batch) = tokio::try_join!(
            self.resolve(slug, Some(bearer)),
            self.resolve(&alias, Some(bearer))
        )?;
        BatchRoute::pair(&live, &batch, filter)
    }

    pub async fn submit_batch(&self, bearer: &Bearer, body: Vec<u8>) -> Result<BatchId, Error> {
        #[derive(Deserialize)]
        struct Accepted {
            id: BatchId,
        }
        self.exchange::<Accepted>(
            self.request(Method::POST, "batches")?
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(body),
            Some(bearer),
            64 * 1024,
        )
        .await
        .map(|accepted| accepted.id)
    }

    pub async fn read_batch(
        &self,
        bearer: &Bearer,
        id: &BatchId,
        custom_id: &str,
    ) -> Result<Option<Settlement>, Error> {
        let job: Job = self
            .exchange(
                self.request(Method::GET, &format!("batches/{}", id.0))?,
                Some(bearer),
                RESULTS_LIMIT,
            )
            .await?;
        if &job.id != id {
            return Err(Error::InvalidResponse);
        }
        let usage = job.usage;
        let settlement = match job.status {
            JobStatus::Validating | JobStatus::InProgress | JobStatus::Finalizing => {
                return Ok(None);
            }
            JobStatus::Completed => {
                let results = job.results.ok_or(Error::InvalidResponse)?;
                let [row]: [Row; 1] = results.try_into().map_err(|_| Error::InvalidResponse)?;
                if row.custom_id != custom_id {
                    return Err(Error::InvalidResponse);
                }
                let response = match (row.response, row.error) {
                    (Some(row), None) if (200..300).contains(&row.status_code) => Ok(row.body),
                    (Some(row), None) => Err(format!(
                        "OpenRouter batch row returned HTTP {}",
                        row.status_code
                    )),
                    (None, Some(error)) => Err(error.message.chars().take(300).collect()),
                    _ => return Err(Error::InvalidResponse),
                };
                Settlement { usage, response }
            }
            JobStatus::Failed | JobStatus::Expired | JobStatus::Cancelled => Settlement {
                usage,
                response: Err(job.error.map_or_else(
                    || "OpenRouter batch ended without a result".into(),
                    |e| e.message.chars().take(300).collect(),
                )),
            },
        };
        Ok(Some(settlement))
    }

    pub async fn delete_batch(&self, bearer: &Bearer, id: &BatchId) -> Result<(), Error> {
        self.exchange::<Value>(
            self.request(Method::DELETE, &format!("batches/{}", id.0))?,
            Some(bearer),
            64 * 1024,
        )
        .await
        .map(|_| ())
    }
}

impl Bearer {
    /// Pins the credential across restart, without storing it or trusting a reusable key name.
    pub fn batch_fingerprint(&self) -> String {
        format!("{:x}", Sha256::digest(self.expose().as_bytes()))
    }
}
