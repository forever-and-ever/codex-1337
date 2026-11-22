//! Prompt-cache facts the app server needs to estimate cache warmth and keep warm caches warm.
//!
//! Core only reports requests that completed, human messages, and changes that break the cached
//! prefix. The app server owns estimates, warming, retention and scheduling.
use crate::client::ModelClient;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::CodexResponsesRequestKind;
use codex_api::AnthropicClient;
use codex_api::AnthropicRequest;
use codex_api::AnthropicWarmingRequest;
use codex_api::Provider;
use codex_api::ReqwestTransport;
use codex_api::map_api_error;
use codex_model_provider::AnthropicAuthContext;
use codex_protocol::ThreadId;
use codex_protocol::error::Result;
use codex_protocol::openai_models::AnthropicCacheTtl;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::turn_input::TurnInput;
use codex_protocol::turn_input::TurnStartOptions;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use tokio::time::Instant;

/// Provider retention policy for passive estimates and supported keepalive replays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarmingLore {
    /// GPT-6-family `OpenAI` requests use the default 30-minute minimum retention.
    /// This deployment adopts that window for API and subscription routes; passive estimates
    /// do not enable request replay.
    OpenAi,
    /// Native Claude: each request declares its TTL, each read refreshes it, and a zero-output
    /// replay keeps it warm.
    Claude { ttl: AnthropicCacheTtl },
}

impl WarmingLore {
    pub fn lifetime(self) -> Duration {
        match self {
            Self::OpenAi => Duration::from_mins(/*secs*/ 30),
            Self::Claude {
                ttl: AnthropicCacheTtl::FiveMinutes,
            } => Duration::from_mins(/*secs*/ 5),
            Self::Claude {
                ttl: AnthropicCacheTtl::OneHour,
            } => Duration::from_hours(/*secs*/ 1),
        }
    }
}

/// Why a completed request cannot be replayed to keep its cache warm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreplayable {
    /// Only native Claude replays a prefix with zero output.
    Provider,
    /// Subagents are never warmed.
    Subagent,
    /// Manual thinking, structured output, forced tool choice or context management forbid a
    /// zero-output replay.
    RequestShape,
    /// The prefix was too short for the provider to cache.
    Uncached,
}

/// A change the app server must see before any request that follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptCacheFact {
    /// A human message starts or steers a turn; it restarts the warming hold.
    HumanTurn,
    /// The next request will not match the cached prefix: a model, effort, provider or account
    /// switch, or replaced history.
    Invalidated,
}

/// A conversation request that completed with usage.
pub struct CompletedRequest {
    /// The invalidation epoch observed at dispatch; a later invalidation makes this stale.
    pub epoch: u64,
    pub started: RequestStart,
    pub lore: WarmingLore,
    pub usage: TokenUsage,
    pub replay: std::result::Result<WarmingRequest, Unreplayable>,
}

/// Observes prompt-cache facts synchronously, so real work always supersedes warming.
pub trait PromptCacheObserver: Send + Sync + Debug {
    fn epoch(&self, thread: ThreadId) -> u64;
    fn fact(&self, thread: ThreadId, fact: PromptCacheFact);
    fn completed(&self, thread: ThreadId, request: CompletedRequest);
}

/// Installs one app-server-owned observer before the manager starts any threads.
pub trait PromptCacheManager {
    fn with_prompt_cache_observer(self, observer: Arc<dyn PromptCacheObserver>) -> Self;
}

/// Immutable wire prefix and transport identity, without retained bearer credentials or runtime.
#[derive(Clone)]
pub struct WarmingRequest {
    request: AnthropicWarmingRequest,
    transport: ReqwestTransport,
    provider: Provider,
    auth: Arc<AnthropicAuthContext>,
    account: String,
}

impl WarmingRequest {
    pub fn retained_bytes(&self) -> usize {
        self.request.retained_bytes()
    }

    /// Sends once, after authentication, unless the caller no longer wants it. Freshness begins
    /// at dispatch.
    pub async fn send(
        &self,
        wanted: impl Fn() -> bool,
    ) -> Result<Option<(RequestStart, TokenUsage)>> {
        let auth = self.auth.capture(&self.account).await?;
        if !wanted() {
            return Ok(None);
        }
        let started = RequestStart::now();
        let usage =
            AnthropicClient::new(self.transport.clone(), self.provider.clone(), auth.auth())
                .warm(&self.request)
                .await
                .map_err(map_api_error)?;
        Ok(Some((started, usage)))
    }
}

/// What a conversation request reports once it completes.
pub(crate) struct Dispatch {
    observer: Arc<dyn PromptCacheObserver>,
    thread: ThreadId,
    epoch: u64,
    started: RequestStart,
    lore: WarmingLore,
    replay: std::result::Result<WarmingRequest, Unreplayable>,
}

impl Dispatch {
    pub(crate) fn completed(self, usage: &TokenUsage) {
        let cached = usage.cached_input_tokens > 0 || usage.cache_write_input_tokens > 0;
        self.observer.completed(
            self.thread,
            CompletedRequest {
                epoch: self.epoch,
                started: self.started,
                lore: self.lore,
                usage: usage.clone(),
                replay: self
                    .replay
                    .and_then(|request| cached.then_some(request).ok_or(Unreplayable::Uncached)),
            },
        );
    }
}

impl ModelClient {
    pub(crate) fn bind_prompt_cache_observer(
        &self,
        observer: Option<Arc<dyn PromptCacheObserver>>,
    ) {
        if let Some(observer) = observer {
            assert!(
                self.prompt_cache_observer.set(observer).is_ok(),
                "one prompt-cache observer per runtime"
            );
        }
    }

    pub(crate) fn observe_prompt_cache(&self, fact: PromptCacheFact) {
        if let Some(observer) = self.prompt_cache_observer.get() {
            observer.fact(self.state.thread_id, fact);
        }
    }

    pub(crate) fn anthropic_cache_ttl(
        &self,
        model: &ModelInfo,
        effort: Option<&ReasoningEffort>,
    ) -> AnthropicCacheTtl {
        self.anthropic_prompt_cache.resolve(
            &model.slug,
            effort.or(model.default_reasoning_level.as_ref()),
        )
    }

    /// Retention follows the configured provider, not the shared Responses wire format.
    pub(crate) fn prompt_cache_lore(
        &self,
        model: &ModelInfo,
        effort: Option<&ReasoningEffort>,
    ) -> Option<WarmingLore> {
        if self.uses_claude_subscription() {
            Some(WarmingLore::Claude {
                ttl: self.anthropic_cache_ttl(model, effort),
            })
        } else if self.state.provider.info().is_openai() {
            Some(WarmingLore::OpenAi)
        } else {
            None
        }
    }

    /// Captures what a conversation request reports when it completes; none when nothing
    /// listens or the provider gives no lore.
    pub(crate) fn prompt_cache_dispatch(
        &self,
        model: &ModelInfo,
        effort: Option<&ReasoningEffort>,
        replay: impl FnOnce() -> std::result::Result<WarmingRequest, Unreplayable>,
    ) -> Option<Dispatch> {
        let observer = self.prompt_cache_observer.get()?;
        let lore = self.prompt_cache_lore(model, effort)?;
        Some(Dispatch {
            observer: Arc::clone(observer),
            thread: self.state.thread_id,
            epoch: observer.epoch(self.state.thread_id),
            started: RequestStart::now(),
            lore,
            replay: if self.state.session_source.is_non_root_agent() {
                Err(Unreplayable::Subagent)
            } else {
                replay()
            },
        })
    }

    /// Only conversation inference refreshes the estimate, not compaction or connection prewarm.
    pub(crate) fn responses_cache_dispatch(
        &self,
        model: &ModelInfo,
        effort: Option<&ReasoningEffort>,
        metadata: &CodexResponsesMetadata,
    ) -> Option<Dispatch> {
        matches!(metadata.request_kind, Some(CodexResponsesRequestKind::Turn))
            .then(|| self.prompt_cache_dispatch(model, effort, || Err(Unreplayable::Provider)))
            .flatten()
    }

    pub(super) fn warming_request(
        &self,
        request: &AnthropicRequest,
        transport: ReqwestTransport,
        provider: Provider,
        account: Option<String>,
    ) -> std::result::Result<WarmingRequest, Unreplayable> {
        let request = match AnthropicWarmingRequest::from_inference(request) {
            Ok(Some(request)) => request,
            Ok(None) => return Err(Unreplayable::RequestShape),
            Err(error) => {
                tracing::debug!(%error, "Cannot build an Anthropic warming request");
                return Err(Unreplayable::RequestShape);
            }
        };
        let auth = self
            .state
            .provider
            .anthropic_auth_context()
            .ok()
            .flatten()
            .ok_or(Unreplayable::Provider)?;
        Ok(WarmingRequest {
            request,
            transport,
            provider,
            auth,
            account: account.ok_or(Unreplayable::Provider)?,
        })
    }
}

/// Only non-empty input typed by a person restarts the warming hold; goal continuations, agent
/// messages and other automatic turns do not.
pub(crate) fn is_human_message<T>(input: &TurnInput<T>, start: &TurnStartOptions) -> bool {
    matches!(input, TurnInput::UserInput { content, .. } if !content.is_empty())
        && matches!(
            start.turn_trigger.as_deref(),
            None | Some("composer" | "realtime")
        )
}

/// Cache TTL includes suspend; the second clock also prevents wall-clock rollback from
/// extending a lease. A reversed wall clock conservatively expires it.
#[derive(Clone, Copy, Debug)]
pub struct RequestStart {
    wall: SystemTime,
    monotonic: Instant,
}

impl RequestStart {
    pub fn now() -> Self {
        Self {
            wall: SystemTime::now(),
            monotonic: Instant::now(),
        }
    }

    pub fn wall(self) -> SystemTime {
        self.wall
    }

    pub fn age(self) -> Duration {
        self.monotonic
            .elapsed()
            .max(self.wall.elapsed().unwrap_or(Duration::MAX))
    }
}
