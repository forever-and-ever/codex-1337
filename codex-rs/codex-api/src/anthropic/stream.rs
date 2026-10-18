use super::AnthropicRequest;
use super::AnthropicStreamError;
use super::AnthropicStreamProgress;
use super::decoder::Decoder;
use crate::ApiError;
use crate::ResponseEvent;
use crate::ResponseStream;
use crate::telemetry::SseTelemetry;
use codex_client::StreamResponse;
use codex_client::TransportError;
use eventsource_stream::EventStreamError;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;

const MAX_STREAM_BYTES: usize = 64_000_000;
const MAX_RESPONSE_DURATION: Duration = Duration::from_secs(3600);

pub(super) fn spawn_stream(
    response: StreamResponse,
    request: AnthropicRequest,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) -> ResponseStream {
    let upstream_request_id = response
        .headers
        .get("request-id")
        .or_else(|| response.headers.get("x-request-id"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let (tx, rx_event) = mpsc::channel(32);
    tokio::spawn(async move {
        let result = timeout(
            MAX_RESPONSE_DURATION,
            process(response, &request, &tx, idle_timeout, telemetry),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ApiError::NativeStream(
                "Anthropic response deadline exceeded".into(),
            ))
        });
        if let Err(error) = result {
            let _ = timeout(idle_timeout, tx.send(Err(error))).await;
        }
    });
    ResponseStream {
        rx_event,
        upstream_request_id,
        interrupt: None,
    }
}

async fn process(
    response: StreamResponse,
    request: &AnthropicRequest,
    tx: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) -> Result<(), ApiError> {
    // This also bounds Eventsource's internal line/event accumulation before
    // JSON parsing, including a peer that never sends an SSE delimiter.
    let mut bytes_read = 0usize;
    let bytes = response.bytes.map(move |chunk| {
        let chunk = chunk?;
        bytes_read = bytes_read.saturating_add(chunk.len());
        if bytes_read > MAX_STREAM_BYTES {
            return Err(TransportError::ResponseTooLarge {
                max_bytes: MAX_STREAM_BYTES,
            });
        }
        Ok(chunk)
    });
    let mut events = bytes.eventsource();
    let mut decoder = Decoder::new(request.tools());
    let mut progress = AnthropicStreamProgress::BeforeOutput;
    loop {
        let start = Instant::now();
        let poll = tokio::select! {
            () = tx.closed() => return Ok(()),
            poll = timeout(idle_timeout, events.next()) => poll,
        };
        if let Some(telemetry) = &telemetry {
            telemetry.on_sse_poll(&poll, start.elapsed());
        }
        let event = match poll {
            Err(error) => {
                return Err(ApiError::AnthropicStream {
                    progress,
                    source: AnthropicStreamError::IdleTimeout(error),
                });
            }
            Ok(Some(Err(error))) => {
                let source = match error {
                    EventStreamError::Transport(error) => AnthropicStreamError::Transport(error),
                    error => AnthropicStreamError::MalformedSse(error),
                };
                return Err(ApiError::AnthropicStream { progress, source });
            }
            Ok(None) => {
                return Err(ApiError::AnthropicStream {
                    progress,
                    source: AnthropicStreamError::Truncated,
                });
            }
            Ok(Some(Ok(event))) => event,
        };
        let decoded = decoder.decode(&event.data)?;
        for event in decoded {
            let delivered_progress = progress.after(&event);
            match timeout(idle_timeout, tx.send(Ok(event))).await {
                Ok(Ok(())) => progress = delivered_progress,
                Ok(Err(_)) => return Ok(()),
                Err(_) => return Err(ApiError::NativeStream("Anthropic consumer stalled".into())),
            }
        }
        if decoder.finished() {
            return Ok(());
        }
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
