//! OpenRouter batch admission and one-row jobs. No submit retry: a lost response can hide a charge.

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
    live_audio: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LiveReason {
    CompactionContainsAudio,
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
            live_audio: live
                .model
                .architecture
                .input_modalities
                .iter()
                .any(|kind| kind == "audio"),
        })
    }

    pub fn dispatch(
        &self,
        request: &ResponsesApiRequest,
        compaction: bool,
    ) -> Result<BatchDispatch, Error> {
        use codex_protocol::models::ContentItem;
        use codex_protocol::models::ResponseItem;
        let inline_audio=request.input.iter().any(|item|matches!(item,ResponseItem::Message{content,..} if content.iter().any(|part|matches!(part,ContentItem::InputAudio{..}))));
        if !inline_audio {
            return Ok(BatchDispatch::Batch);
        }
        // OpenRouter explicitly excludes inline audio from Batch. Never upload it or
        // change payer; only required compaction may use the verified live twin.
        if compaction && self.live_audio {
            Ok(BatchDispatch::NecessaryLive(
                LiveReason::CompactionContainsAudio,
            ))
        } else {
            Err(Error::BatchUnavailable)
        }
    }

    /// The serialized order is a wire contract: OpenRouter reads routing fields before requests.
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

#[derive(Debug, Deserialize)]
struct Job {
    id: BatchId,
    #[serde(flatten)]
    state: JobState,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum JobState {
    Validating,
    InProgress,
    Finalizing,
    Completed {
        results: Vec<Row>,
        usage: Option<BatchUsage>,
    },
    Failed {
        error: Option<RowError>,
        usage: Option<BatchUsage>,
    },
    Expired {
        error: Option<RowError>,
        usage: Option<BatchUsage>,
    },
    Cancelled {
        error: Option<RowError>,
        usage: Option<BatchUsage>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchUsage {
    pub prompt_tokens: i64,
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
        #[derive(Deserialize)]
        struct Usage {
            input_tokens: i64,
            output_tokens: i64,
            total_tokens: i64,
            cost: Option<Usd>,
        }
        let usage = body
            .get("usage")
            .cloned()
            .filter(|v| !v.is_null())
            .map(serde_json::from_value::<Usage>)
            .transpose()
            .map_err(|_| Error::InvalidResponse)?
            .map(|u| BatchUsage {
                prompt_tokens: u.input_tokens,
                completion_tokens: u.output_tokens,
                total_tokens: u.total_tokens,
                cost: u.cost,
            });
        Ok(Settlement {
            usage,
            response: Ok(body),
        })
    }
    pub(super) async fn admitted_models(&self, bearer: &Bearer) -> Result<BTreeSet<String>, Error> {
        if !self.admits_global_batch() {
            return Err(Error::BatchUnavailable);
        }
        #[derive(Deserialize)]
        struct UserModel {
            id: String,
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
        let settlement = match job.state {
            JobState::Validating | JobState::InProgress | JobState::Finalizing => return Ok(None),
            JobState::Completed { results, usage } => {
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
            JobState::Failed { error, usage }
            | JobState::Expired { error, usage }
            | JobState::Cancelled { error, usage } => Settlement {
                usage,
                response: Err(error.map_or_else(
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
