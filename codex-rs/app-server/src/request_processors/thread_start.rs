use super::super::thread_lifecycle::ListenerTaskContext;
use super::super::thread_lifecycle::ensure_conversation_listener;
use super::super::thread_lifecycle::ensure_listener_task_running;
use super::super::thread_lifecycle::log_listener_attach_result;
use super::internal_error;
use crate::outgoing_message::ConnectionRequestId;
use crate::outgoing_message::RequestContext;
use crate::thread_state::ThreadRuntimeLease;
use codex_app_server_protocol::ConfigWarningNotification;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadStartResponse;
use codex_protocol::ThreadId;
use codex_protocol::protocol::W3cTraceContext;
use std::future::Future;
use tracing::Instrument;
use tracing::Span;

pub(super) trait ThreadStartOrigin: Send + Sync + 'static {
    type RuntimePin: Send + 'static;

    fn span(&self) -> Span;
    fn request_trace(&self) -> Option<W3cTraceContext>;

    fn warn(
        &self,
        context: &ListenerTaskContext,
        notification: ConfigWarningNotification,
    ) -> impl Future<Output = ()> + Send;

    fn register_client(
        &self,
        context: &ListenerTaskContext,
        thread_id: ThreadId,
    ) -> impl Future<Output = ()> + Send;

    fn observe_runtime(
        &self,
        context: &ListenerTaskContext,
        thread_id: ThreadId,
    ) -> impl Future<Output = Result<Self::RuntimePin, JSONRPCErrorError>> + Send;

    fn publish(
        &self,
        context: &ListenerTaskContext,
        response: &ThreadStartResponse,
        thread_originator: String,
    ) -> impl Future<Output = ()> + Send;
}

pub(super) struct FrontendStart {
    request_id: ConnectionRequestId,
    span: Span,
    request_trace: Option<W3cTraceContext>,
    experimental_raw_events: bool,
}

impl FrontendStart {
    pub(super) fn new(
        request_id: ConnectionRequestId,
        request_context: &RequestContext,
        experimental_raw_events: bool,
    ) -> Self {
        Self {
            request_id,
            span: request_context.span(),
            request_trace: request_context.request_trace(),
            experimental_raw_events,
        }
    }
}

impl ThreadStartOrigin for FrontendStart {
    type RuntimePin = ();

    fn span(&self) -> Span {
        self.span.clone()
    }

    fn request_trace(&self) -> Option<W3cTraceContext> {
        self.request_trace.clone()
    }

    async fn warn(&self, context: &ListenerTaskContext, notification: ConfigWarningNotification) {
        context
            .outgoing
            .send_server_notification_to_connections(
                &[self.request_id.connection_id],
                ServerNotification::ConfigWarning(notification),
            )
            .await;
    }

    async fn register_client(&self, context: &ListenerTaskContext, thread_id: ThreadId) {
        context
            .thread_state_manager
            .try_add_connection_to_thread(thread_id, self.request_id.connection_id)
            .await;
    }

    async fn observe_runtime(
        &self,
        context: &ListenerTaskContext,
        thread_id: ThreadId,
    ) -> Result<(), JSONRPCErrorError> {
        log_listener_attach_result(
            ensure_conversation_listener(
                context.clone(),
                thread_id,
                self.request_id.connection_id,
                self.experimental_raw_events,
            )
            .instrument(tracing::info_span!(
                "app_server.thread_start.attach_listener",
                otel.name = "app_server.thread_start.attach_listener",
                thread_start.experimental_raw_events = self.experimental_raw_events,
            ))
            .await,
            thread_id,
            self.request_id.connection_id,
            "thread",
        );
        Ok(())
    }

    async fn publish(
        &self,
        context: &ListenerTaskContext,
        response: &ThreadStartResponse,
        thread_originator: String,
    ) {
        context
            .outgoing
            .send_response_with_thread_originator(
                self.request_id.clone(),
                response.clone(),
                thread_originator,
            )
            .instrument(tracing::info_span!(
                "app_server.thread_start.send_response",
                otel.name = "app_server.thread_start.send_response",
            ))
            .await;
    }
}

pub(super) struct HeadlessStart;

impl ThreadStartOrigin for HeadlessStart {
    type RuntimePin = ThreadRuntimeLease;

    fn span(&self) -> Span {
        Span::current()
    }

    fn request_trace(&self) -> Option<W3cTraceContext> {
        None
    }

    async fn warn(&self, _context: &ListenerTaskContext, notification: ConfigWarningNotification) {
        tracing::warn!(summary = %notification.summary, "headless thread startup warning");
    }

    async fn register_client(&self, _context: &ListenerTaskContext, _thread_id: ThreadId) {}

    async fn observe_runtime(
        &self,
        context: &ListenerTaskContext,
        thread_id: ThreadId,
    ) -> Result<ThreadRuntimeLease, JSONRPCErrorError> {
        let lease = context.thread_state_manager.lease_runtime(thread_id).await;
        let runtime = context
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|err| internal_error(format!("new thread disappeared: {err}")))?;
        let state = context.thread_state_manager.thread_state(thread_id).await;
        ensure_listener_task_running(context.clone(), thread_id, runtime, state).await?;
        Ok(lease)
    }

    async fn publish(
        &self,
        _context: &ListenerTaskContext,
        _response: &ThreadStartResponse,
        _thread_originator: String,
    ) {
    }
}
