use super::*;
use crate::anthropic::AnthropicClient;
use crate::anthropic::AnthropicStreamOptions;
use crate::anthropic::AnthropicTool;
use crate::anthropic::AnthropicToolIdentity;
use crate::auth::AuthProvider;
use crate::provider::Provider;
use crate::provider::RetryConfig;
use bytes::Bytes;
use codex_client::HttpTransport;
use codex_client::Request;
use codex_client::Response;
use codex_protocol::error::CodexErrKind;
use codex_protocol::error::CodexErrorDetails;
use futures::stream;
use http::HeaderMap;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

fn request() -> AnthropicRequest {
    AnthropicRequest::from_body(
        json!({"model":"synthetic","stream":true}),
        Arc::from([]),
        Arc::from([]),
    )
    .unwrap()
}

fn response(chunks: Vec<Vec<u8>>) -> StreamResponse {
    StreamResponse {
        status: http::StatusCode::OK,
        headers: HeaderMap::new(),
        bytes: Box::pin(stream::iter(
            chunks.into_iter().map(|chunk| Ok(Bytes::from(chunk))),
        )),
    }
}

#[tokio::test]
async fn fragmented_utf8_crlf_sse_and_terminal_failures_are_not_replayed() {
    let start = "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\",\"usage\":{\"input_tokens\":13,\"output_tokens\":1,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":7}}}\r\n\r\n";
    let text = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\r\n\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"α answer\"}}\r\n\r\ndata: {\"type\":\"content_block_stop\",\"index\":0}\r\n\r\n";
    for (tail, expected) in [
        (
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4,\"cache_read_input_tokens\":22000,\"cache_creation_input_tokens\":31}}\r\n\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n",
            "completed",
        ),
        (
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"synthetic overload\"}}\r\n\r\n",
            "error",
        ),
        ("", "truncated"),
    ] {
        let wire = format!("{start}{text}{tail}");
        // A single-byte chunk splits UTF-8 and every SSE delimiter.
        for chunk_size in [wire.len(), 1] {
            let chunks = wire
                .as_bytes()
                .chunks(chunk_size)
                .map(<[u8]>::to_vec)
                .collect();
            let mut stream = spawn_stream(
                response(chunks),
                request(),
                Duration::from_secs(1),
                /*telemetry*/ None,
            );
            let mut observed = Vec::new();
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ResponseEvent::OutputItemDone(item)) => {
                        observed.push(serde_json::to_value(item).unwrap())
                    }
                    Ok(ResponseEvent::Completed {
                        token_usage,
                        end_turn,
                        ..
                    }) => {
                        assert_eq!(expected, "completed");
                        assert_eq!(end_turn, Some(true));
                        assert_eq!(token_usage.unwrap().total_tokens, 22048);
                        observed.push(json!("completed"));
                    }
                    Err(error) => {
                        assert!(matches!(
                            error,
                            ApiError::NativeStream(_)
                                | ApiError::AnthropicStream {
                                    progress: AnthropicStreamProgress::CompletedItem,
                                    source: AnthropicStreamError::Truncated,
                                }
                        ));
                        let mapped = crate::map_api_error(error);
                        assert!(matches!(mapped.details(), CodexErrorDetails::Fatal(_)));
                        assert_eq!(mapped.retry_delay(/*retry_count*/ 1), None);
                        observed.push(json!(expected));
                    }
                    _ => {}
                }
            }
            assert_eq!(
                observed,
                vec![
                    json!({"type":"message","id":"msg_0","role":"assistant","content":[{"type":"output_text","text":"α answer"}]}),
                    json!(expected)
                ]
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_stream_and_dropped_consumer_release_transport() {
    struct Dropped(Arc<AtomicUsize>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    for expected_progress in [
        AnthropicStreamProgress::BeforeOutput,
        AnthropicStreamProgress::PartialOutput,
        AnthropicStreamProgress::CompletedItem,
    ] {
        let mut frames = vec![json!({
            "type":"message_start","message":{"id":"msg","usage":{"input_tokens":1,"output_tokens":1}}
        })];
        if expected_progress != AnthropicStreamProgress::BeforeOutput {
            frames.push(json!({
                "type":"content_block_start","index":0,
                "content_block":{"type":"thinking","thinking":"partial","signature":"signed"}
            }));
        }
        if expected_progress == AnthropicStreamProgress::CompletedItem {
            frames.push(json!({"type":"content_block_stop","index":0}));
        }
        let prefix = frames
            .into_iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect::<String>();
        for consume in [true, false] {
            let count = Arc::new(AtomicUsize::new(0));
            let guard = Dropped(count.clone());
            let bytes = stream::unfold(guard, |guard| async move {
                std::future::pending::<()>().await;
                Some((Ok(Bytes::new()), guard))
            });
            let response = StreamResponse {
                status: http::StatusCode::OK,
                headers: HeaderMap::new(),
                bytes: Box::pin(stream::iter([Ok(Bytes::from(prefix.clone()))]).chain(bytes)),
            };
            let mut stream = spawn_stream(
                response,
                request(),
                Duration::from_secs(1),
                /*telemetry*/ None,
            );
            if consume {
                let error = loop {
                    if let Err(error) = stream.next().await.expect("idle failure") {
                        break error;
                    }
                };
                let ApiError::AnthropicStream {
                    progress,
                    source: AnthropicStreamError::IdleTimeout(_),
                } = &error
                else {
                    panic!("expected typed idle timeout: {error:?}");
                };
                assert_eq!(*progress, expected_progress);
                assert!(std::error::Error::source(&error).is_some());
                assert_eq!(
                    crate::map_api_error(error).retry_delay(1).is_some(),
                    expected_progress != AnthropicStreamProgress::CompletedItem
                );
                assert!(stream.next().await.is_none());
            } else {
                drop(stream);
                tokio::task::yield_now().await;
            }
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }
}

struct NoAuth;
impl AuthProvider for NoAuth {
    fn add_auth_headers(&self, _: &mut HeaderMap) {}
}

struct UncertainTransport(Arc<AtomicUsize>);
impl HttpTransport for UncertainTransport {
    async fn execute(&self, _: Request) -> Result<Response, TransportError> {
        unreachable!("native inference must stream")
    }
    async fn stream(&self, _: Request) -> Result<StreamResponse, TransportError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(TransportError::Network("synthetic uncertain POST".into()))
    }
}

#[tokio::test]
async fn uncertain_post_attempts_once_despite_provider_retry_policy() {
    let count = Arc::new(AtomicUsize::new(0));
    let provider = Provider {
        name: "synthetic".into(),
        base_url: "https://fixture.invalid/v1".into(),
        query_params: None,
        headers: HeaderMap::new(),
        retry: RetryConfig {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            retry_429: true,
            retry_5xx: true,
            retry_transport: true,
        },
        stream_idle_timeout: Duration::from_secs(1),
    };
    let client = AnthropicClient::<UncertainTransport>::new(
        UncertainTransport(count.clone()),
        provider,
        Arc::new(NoAuth),
    );
    assert!(matches!(
        client
            .stream_request(request(), AnthropicStreamOptions::default())
            .await,
        Err(ApiError::NativeStream(_))
    ));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn errors_before_any_completed_item_stay_typed_for_retry() {
    // Provider frames and recoverable body interruptions use core's precommit
    // retry/discard boundary; completed native items must never be replayed.
    let start = "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\",\"usage\":{\"input_tokens\":13,\"output_tokens\":1,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":7}}}\n\n";
    let partial = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n";
    let mut kinds = Vec::new();
    for error_type in [
        "overloaded_error",
        "rate_limit_error",
        "api_error",
        "invalid_request_error",
    ] {
        let error = format!(
            "data: {{\"type\":\"error\",\"error\":{{\"type\":\"{error_type}\",\"message\":\"synthetic\"}}}}\n\n"
        );
        let wire = format!("{start}{partial}{error}").into_bytes();
        let mut stream = spawn_stream(
            response(vec![wire]),
            request(),
            Duration::from_secs(1),
            /*telemetry*/ None,
        );
        while let Some(event) = stream.next().await {
            if let Err(error) = event {
                kinds.push(CodexErrKind::from(crate::map_api_error(error).details()));
            }
        }
    }
    assert_eq!(
        kinds,
        vec![
            CodexErrKind::ServerOverloaded,
            CodexErrKind::RateLimitExceeded,
            CodexErrKind::Stream,
            CodexErrKind::Fatal,
        ]
    );
    // A timeout, clean EOF and malformed SSE can all arrive after HTTP succeeds;
    // only precompletion transport interruption may spend the finite retry budget.
    let stop = "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n";
    for block in [
        json!({"type":"text","text":"partial"}),
        json!({"type":"thinking","thinking":"partial","signature":"signed"}),
        json!({"type":"tool_use","id":"call","name":"fixture","input":{"value":1}}),
    ] {
        let block_start = format!(
            "data: {}\n\n",
            json!({
                "type":"content_block_start","index":0,"content_block":block
            })
        );
        for (prefix, expected_progress) in [
            (start.to_owned(), AnthropicStreamProgress::BeforeOutput),
            (
                format!("{start}{block_start}"),
                AnthropicStreamProgress::PartialOutput,
            ),
            (
                format!(
                    "{start}{block_start}{stop}data: {}\n\n",
                    json!({
                        "type":"content_block_start","index":1,
                        "content_block":{"type":"text","text":"later partial"}
                    })
                ),
                AnthropicStreamProgress::CompletedItem,
            ),
        ] {
            for terminal in [
                None,
                Some(Err(TransportError::Timeout)),
                Some(Err(TransportError::Network(
                    "synthetic body disconnect".into(),
                ))),
                Some(Err(TransportError::ResponseTooLarge {
                    max_bytes: MAX_STREAM_BYTES,
                })),
                Some(Ok(Bytes::from_static(b"data: \xff\n\n"))),
                Some(Ok(Bytes::from_static(b"data: not-json\n\n"))),
            ] {
                let recoverable = matches!(
                    &terminal,
                    None | Some(Err(TransportError::Timeout | TransportError::Network(_)))
                );
                let malformed_json = matches!(&terminal, Some(Ok(bytes)) if bytes.as_ref() == b"data: not-json\n\n".as_slice());
                let chunks =
                    stream::iter(std::iter::once(Ok(Bytes::from(prefix.clone()))).chain(terminal));
                let body = StreamResponse {
                    status: http::StatusCode::OK,
                    headers: HeaderMap::new(),
                    bytes: Box::pin(chunks),
                };
                let request = AnthropicRequest::from_body(
                    json!({"model":"synthetic","stream":true}),
                    Arc::from([AnthropicTool {
                        name: "fixture".into(),
                        description: String::new(),
                        input_schema: json!({"type":"object","properties":{"value":{"type":"integer"}}}),
                        identity: AnthropicToolIdentity::Function {
                            name: "fixture".into(),
                            namespace: None,
                        },
                    }]),
                    Arc::from([]),
                ).unwrap();
                let mut stream = spawn_stream(
                    body,
                    request,
                    Duration::from_secs(1),
                    /*telemetry*/ None,
                );
                let mut failure = None;
                let mut completed = 0;
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(ResponseEvent::OutputItemDone(_)) => completed += 1,
                        Err(error) => {
                            assert!(failure.replace(error).is_none());
                        }
                        _ => {}
                    }
                }
                assert_eq!(
                    completed,
                    usize::from(expected_progress == AnthropicStreamProgress::CompletedItem)
                );
                let error = failure.expect("terminal stream failure");
                if malformed_json {
                    assert!(matches!(error, ApiError::NativeStream(_)));
                } else {
                    let ApiError::AnthropicStream { progress, .. } = &error else {
                        panic!("expected typed body failure: {error:?}");
                    };
                    assert_eq!(*progress, expected_progress);
                }
                let mapped = crate::map_api_error(error);
                let retryable =
                    recoverable && expected_progress != AnthropicStreamProgress::CompletedItem;
                assert_eq!(mapped.retry_delay(1).is_some(), retryable);
                assert_eq!(
                    CodexErrKind::from(mapped.details()),
                    if retryable {
                        CodexErrKind::Stream
                    } else {
                        CodexErrKind::Fatal
                    }
                );
            }
        }
    }
}
