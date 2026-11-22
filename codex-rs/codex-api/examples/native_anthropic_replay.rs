//! Offline native Messages replay. Input and output follow `scripts/native_anthropic/README.md`.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use bytes::Bytes;
use codex_api::AnthropicCachePolicy;
use codex_api::AnthropicClient;
use codex_api::AnthropicOperation;
use codex_api::AnthropicPrompt;
use codex_api::AnthropicRequest;
use codex_api::AnthropicRequestOptions;
use codex_api::AnthropicStreamError;
use codex_api::AnthropicStreamOptions;
use codex_api::AnthropicTool;
use codex_api::AnthropicToolIdentity;
use codex_api::ApiError;
use codex_api::AuthError;
use codex_api::AuthProvider;
use codex_api::Provider;
use codex_api::ResponseEvent;
use codex_api::RetryConfig;
use codex_api::anthropic_function_input_schema;
use codex_api::encode_anthropic_request;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::Request;
use codex_client::RequestBody;
use codex_client::Response;
use codex_client::StreamResponse;
use codex_client::TransportError;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use futures::StreamExt;
use futures::stream;
use http::HeaderMap;
use http::Method;
use http::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::future::ready;
use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

const MAX_BYTES: usize = 256 * 1024;
const MAX_CASES: usize = 16;

#[derive(Deserialize)]
struct Case {
    id: String,
    model: String,
    request: Value,
    frames: Vec<Value>,
}

struct NoAuth;
impl AuthProvider for NoAuth {
    fn add_auth_headers(&self, _: &mut HeaderMap) -> Result<(), AuthError> {
        Ok(())
    }
}

struct Offline {
    chunks: Vec<Vec<u8>>,
    expected: EncodedJsonBody,
    calls: Arc<AtomicUsize>,
}

impl HttpTransport for Offline {
    fn execute(&self, _: Request) -> impl Future<Output = Result<Response, TransportError>> {
        ready(Err(TransportError::Build(
            "offline adapter forbids unary requests".into(),
        )))
    }

    fn stream(
        &self,
        request: Request,
    ) -> impl Future<Output = Result<StreamResponse, TransportError>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) != 0
            || request.method != Method::POST
            || request.url != "https://fixture.invalid/v1/messages"
            || request.headers.len() > 8
            || request.headers.contains_key("authorization")
            || request.headers.contains_key("cookie")
            || !matches!(request.body.as_ref(), Some(RequestBody::EncodedJson(body)) if body.as_bytes() == self.expected.as_bytes())
        {
            return ready(Err(TransportError::Build(
                "unexpected offline Messages request".into(),
            )));
        }
        ready(Ok(StreamResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(stream::iter(
                self.chunks
                    .clone()
                    .into_iter()
                    .map(|chunk| Ok(Bytes::from(chunk))),
            )),
        }))
    }
}

fn tools(
    declarations: &[Value],
    namespace: Option<&str>,
    output: &mut Vec<AnthropicTool>,
) -> Result<()> {
    for declaration in declarations {
        let name = declaration["name"].as_str().context("tool name missing")?;
        let kind = declaration["type"].as_str().context("tool type missing")?;
        if kind == "namespace" {
            ensure!(namespace.is_none(), "nested tool namespace");
            tools(
                declaration["tools"]
                    .as_array()
                    .context("namespace tools missing")?,
                Some(name),
                output,
            )?;
            continue;
        }
        let identity = match kind {
            "function" => AnthropicToolIdentity::Function {
                name: name.to_owned(),
                namespace: namespace.map(str::to_owned),
            },
            "custom" => AnthropicToolIdentity::Custom {
                name: name.to_owned(),
                namespace: namespace.map(str::to_owned),
            },
            _ => bail!("unsupported fixture tool {kind}"),
        };
        let input_schema = if kind == "custom" {
            json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"]})
        } else {
            anthropic_function_input_schema(declaration["parameters"].clone())?
        };
        output.push(AnthropicTool {
            name: identity.wire_alias()?,
            description: declaration["description"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            input_schema,
            identity,
        });
    }
    Ok(())
}

fn encode(case: &Case, model: &ModelInfo) -> Result<AnthropicRequest> {
    let input = match &case.request["input"] {
        Value::String(text) => {
            json!([{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}])
        }
        Value::Array(_) => case.request["input"].clone(),
        _ => bail!("fixture input must be text or history"),
    };
    let input: Vec<ResponseItem> = serde_json::from_value(input)?;
    let mut declarations = Vec::new();
    if let Some(array) = case.request["tools"].as_array() {
        tools(array, /*namespace*/ None, &mut declarations)?;
    }
    let declarations = Arc::from(declarations);
    let effort = case
        .request
        .pointer("/reasoning/effort")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    let max_output = case.request["max_output_tokens"]
        .as_u64()
        .context("output cap missing")?;
    let max_output_tokens =
        NonZeroU32::new(u32::try_from(max_output)?).context("zero output cap")?;
    Ok(encode_anthropic_request(
        &AnthropicPrompt {
            input: &input,
            instructions: case.request["instructions"].as_str().unwrap_or_default(),
            tools: &declarations,
            parallel_tool_calls: true,
            output_schema: None,
        },
        &AnthropicRequestOptions {
            model,
            reasoning_effort: effort.as_ref(),
            max_output_tokens,
            cache: AnthropicCachePolicy::Disabled,
            operation: if case.request.get("compaction").is_some() {
                AnthropicOperation::Compact
            } else {
                AnthropicOperation::Inference
            },
        },
    )?)
}

async fn replay(case: &Case, request: &AnthropicRequest, chunks: Vec<Vec<u8>>) -> Result<Value> {
    let calls = Arc::new(AtomicUsize::new(0));
    let client = AnthropicClient::new(
        Offline {
            chunks,
            expected: EncodedJsonBody::encode(request.body())?,
            calls: Arc::clone(&calls),
        },
        Provider {
            name: "offline-native-anthropic".into(),
            base_url: "https://fixture.invalid/v1".into(),
            query_params: None,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 0,
                base_delay: Duration::ZERO,
                retry_429: false,
                retry_5xx: false,
                retry_transport: false,
            },
            stream_idle_timeout: Duration::from_secs(/*secs*/ 1),
        },
        Arc::new(NoAuth),
    );
    let mut events = client
        .stream_request(request.clone(), AnthropicStreamOptions::default())
        .await?;
    let mut output = Vec::new();
    let mut usage = Value::Null;
    let mut terminal = "truncated";
    let mut error = None;
    let mut terminal_seen = false;
    let mut event_trace = Vec::new();
    while let Some(event) = events.next().await {
        event_trace.push(format!("{event:?}"));
        ensure!(!terminal_seen, "event after terminal outcome");
        match event {
            Ok(ResponseEvent::OutputItemDone(item)) => output.push(serde_json::to_value(item)?),
            Ok(ResponseEvent::Completed {
                token_usage,
                usage_metadata,
                ..
            }) => {
                terminal_seen = true;
                terminal = if usage_metadata
                    .and_then(|metadata| metadata.metadata)
                    .is_some_and(|metadata| metadata["stop_reason"] == "max_tokens")
                {
                    "incomplete"
                } else {
                    "completed"
                };
                let tokens = token_usage.context("completed response omitted usage")?;
                usage = json!({"uncached_input": tokens.input_tokens - tokens.cached_input_tokens - tokens.cache_write_input_tokens,
                    "cache_write":tokens.cache_write_input_tokens,"cache_read":tokens.cached_input_tokens,"output":tokens.output_tokens});
            }
            Err(failure) => {
                terminal_seen = true;
                terminal = match &failure {
                    ApiError::AnthropicStream {
                        source: AnthropicStreamError::Truncated,
                        ..
                    } => "truncated",
                    _ => "error",
                };
                error = Some(failure.to_string());
            }
            Ok(_) => {}
        }
    }
    ensure!(
        calls.load(Ordering::SeqCst) == 1,
        "native replay must dispatch exactly once"
    );
    Ok(
        json!({"id":case.id,"request":serde_json::from_str::<Value>(request.body().get())?,
        "output":output,"usage":usage,"terminal":terminal,"error":error,"event_trace":event_trace}),
    )
}

fn native_frames(frames: &[Value], tools: &[AnthropicTool]) -> Result<Vec<Value>> {
    // The shared corpus describes canonical function arguments. Only the native
    // simulated upstream envelopes them; custom {input} and canonical captures stay exact.
    let mut functions: HashMap<u64, Option<usize>> = HashMap::new();
    let delta_indices: HashSet<u64> = frames
        .iter()
        .filter(|frame| {
            frame["type"] == "content_block_delta" && frame["delta"]["type"] == "input_json_delta"
        })
        .filter_map(|frame| frame["index"].as_u64())
        .collect();
    let mut output: Vec<Value> = Vec::new();
    for mut frame in frames.iter().cloned() {
        let index = frame["index"].as_u64();
        match frame["type"].as_str() {
            Some("content_block_start")
                if frame["content_block"]["type"] == "tool_use"
                    && tools.iter().any(|tool| {
                        frame["content_block"]["name"] == tool.name
                            && matches!(tool.identity, AnthropicToolIdentity::Function { .. })
                    }) =>
            {
                let index = index.context("tool block index missing")?;
                if !delta_indices.contains(&index) || frame["content_block"]["input"] != json!({}) {
                    frame["content_block"]["input"] =
                        json!({"arguments":frame["content_block"]["input"]});
                }
                functions.insert(index, None);
            }
            Some("content_block_delta") if frame["delta"]["type"] == "input_json_delta" => {
                if let Some(last) = index.and_then(|index| functions.get_mut(&index)) {
                    if last.is_none() {
                        let partial = frame["delta"]["partial_json"]
                            .as_str()
                            .context("tool delta is not text")?;
                        frame["delta"]["partial_json"] =
                            json!(format!("{{\"arguments\":{partial}"));
                    }
                    *last = Some(output.len());
                }
            }
            Some("content_block_stop") => {
                if let Some(Some(last)) = index.and_then(|index| functions.remove(&index)) {
                    let partial = output[last]["delta"]["partial_json"]
                        .as_str()
                        .context("tool delta is not text")?;
                    output[last]["delta"]["partial_json"] = json!(format!("{partial}}}"));
                }
            }
            _ => {}
        }
        output.push(frame);
    }
    Ok(output)
}

async fn capture(case: &Case, models: &[ModelInfo]) -> Result<Value> {
    ensure!(case.frames.len() <= 64, "frame bound exceeded");
    let model = models
        .iter()
        .find(|model| model.slug == case.model)
        .context("fixture model not found")?;
    let request = encode(case, model)?;
    ensure!(
        request.body().get().len() <= MAX_BYTES,
        "encoded request bound exceeded"
    );
    let mut frames = Vec::new();
    for frame in native_frames(&case.frames, request.tools())? {
        let kind = frame["type"].as_str().context("frame type missing")?;
        frames.push(format!("event: {kind}\ndata: {frame}\n\n").into_bytes());
    }
    let wire: Vec<u8> = frames.iter().flatten().copied().collect();
    ensure!(wire.len() <= MAX_BYTES, "upstream body bound exceeded");
    let whole = replay(case, &request, frames).await?;
    for chunk_size in [1, 7, 31] {
        let fragmented = replay(
            case,
            &request,
            wire.chunks(chunk_size).map(<[u8]>::to_vec).collect(),
        )
        .await?;
        ensure!(
            fragmented == whole,
            "{} changed under {chunk_size}-byte transport fragmentation",
            case.id
        );
    }
    Ok(whole)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: native_anthropic_replay MODEL_INFO_JSON < corpus.jsonl")?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "model fixture bound exceeded");
    let models: Vec<ModelInfo> = serde_json::from_slice(&bytes)?;
    let stdin = std::io::stdin();
    let mut source = stdin.lock();
    let mut stdout = std::io::stdout().lock();
    let mut ids = HashSet::new();
    loop {
        let mut line = Vec::new();
        if source
            .by_ref()
            .take((MAX_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?
            == 0
        {
            break;
        }
        ensure!(
            line.len() <= MAX_BYTES && ids.len() < MAX_CASES,
            "corpus bounds exceeded"
        );
        let case: Case = serde_json::from_slice(&line)?;
        ensure!(ids.insert(case.id.clone()), "duplicate fixture ID");
        let value = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), capture(&case, &models))
            .await??;
        let capture = serde_json::to_vec(&value)?;
        ensure!(capture.len() <= MAX_BYTES, "capture bound exceeded");
        stdout.write_all(&capture)?;
        stdout.write_all(b"\n")?;
    }
    Ok(())
}
