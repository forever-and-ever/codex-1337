use super::AnthropicRequest;
use super::stream::spawn_stream;
use crate::ApiError;
use crate::ResponseStream;
use crate::auth::SharedAuthProvider;
use crate::endpoint::EndpointSession;
use crate::provider::Provider;
use crate::telemetry::SseTelemetry;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

pub struct AnthropicClient<T: HttpTransport> {
    pub(super) session: EndpointSession<T>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
}

#[derive(Default)]
pub struct AnthropicStreamOptions {
    pub extra_headers: HeaderMap,
}

impl<T: HttpTransport> AnthropicClient<T> {
    pub fn new(transport: T, mut provider: Provider, auth: SharedAuthProvider) -> Self {
        // An uncertain POST may already have incurred usage or emitted tools.
        // Reconnection is not a safe retry mechanism for Messages inference.
        provider.retry.max_attempts = 0;
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
        }
    }

    #[tracing::instrument(name = "anthropic.stream_request", level = "info", skip_all,
        fields(transport = "anthropic_http", http.method = "POST", api.path = "/messages"))]
    pub async fn stream_request(
        &self,
        request: AnthropicRequest,
        options: AnthropicStreamOptions,
    ) -> Result<ResponseStream, ApiError> {
        self.stream_request_with_body_observer(request, options, |_| ())
            .await
    }

    pub async fn stream_request_with_body_observer(
        &self,
        request: AnthropicRequest,
        options: AnthropicStreamOptions,
        observe_body: impl FnOnce(&EncodedJsonBody) + Send,
    ) -> Result<ResponseStream, ApiError> {
        let idle_timeout = self
            .session
            .provider()
            .stream_idle_timeout
            .clamp(Duration::from_secs(1), Duration::from_mins(10));
        let body =
            EncodedJsonBody::encode(request.body()).map_err(|e| ApiError::InvalidRequest {
                message: format!("failed to encode Anthropic request: {e}"),
            })?;
        let headers = request.api_headers(options.extra_headers)?;
        observe_body(&body);
        let response = timeout(
            idle_timeout,
            self.session.stream_encoded_json_with(
                Method::POST,
                "/messages",
                headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                },
            ),
        )
        .await
        .map_err(|_| ApiError::NativeStream("Anthropic request handshake timeout".into()))?
        .map_err(|error| match error {
            // 529 is Anthropic's overload status, unknown to the shared HTTP error bridge.
            ApiError::Transport(codex_client::TransportError::Http {
                status,
                retry_after,
                ..
            }) if status.as_u16() == 529 => ApiError::ServerOverloaded { retry_after },
            ApiError::Transport(codex_client::TransportError::Http { .. }) => error,
            error => ApiError::NativeStream(format!("Anthropic request outcome unknown: {error}")),
        })?;
        if !response.status.is_success() {
            return Err(ApiError::Api {
                status: response.status,
                message: "Anthropic Messages rejected request".into(),
            });
        }
        Ok(spawn_stream(
            response,
            request,
            idle_timeout,
            self.sse_telemetry.clone(),
        ))
    }
}
