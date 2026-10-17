//! Typed UI continuations carried by the existing server-operation ledger.

use super::*;
use codex_app_server_client::AppServerRequestHandle;
use futures::future::BoxFuture;
use std::future::Future;
use std::sync::Mutex as SyncMutex;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
pub(in crate::app) enum UiRequestScope {
    Connection,
    Thread(ThreadId),
    DisplayedThread(ThreadId),
}

#[derive(Clone, Copy)]
pub(in crate::app) enum UiRequestKind {
    ReadOnly,
    Mutation,
}

#[derive(Clone)]
pub(in crate::app) struct UiRequestHandle(CancellationToken);

impl UiRequestHandle {
    /// Stops local work and suppresses its continuation, including an already
    /// delivered result. Cancelling a mutation cannot undo the remote write:
    /// its ledger lane remains uncertain until reconciliation.
    pub(in crate::app) fn cancel(&self) {
        self.0.cancel();
    }
}

type Apply<T> = Box<
    dyn FnOnce(&mut App, &mut AppServerSession, Result<T, &SubmissionFailure>) -> Result<()> + Send,
>;

struct Completion<T> {
    scope: UiRequestScope,
    cancelled: CancellationToken,
    label: &'static str,
    apply: SyncMutex<Option<Apply<T>>>,
}

impl<T: Send + 'static> Completion<T> {
    fn finish(
        &self,
        app: &mut App,
        server: &mut AppServerSession,
        result: Result<T, &SubmissionFailure>,
    ) -> Result<()> {
        let apply = self
            .apply
            .lock()
            .expect("UI continuation owner poisoned")
            .take()
            .expect("UI continuation completed twice");
        if let UiRequestScope::DisplayedThread(thread_id) = self.scope
            && app.current_displayed_thread_id() != Some(thread_id)
        {
            tracing::debug!(operation = self.label, %thread_id,
                "dropping UI continuation for a no-longer-displayed thread");
            return Ok(());
        }
        if self.cancelled.is_cancelled() {
            tracing::debug!(operation = self.label, "dropping cancelled UI continuation");
            return Ok(());
        }
        apply(app, server, result)
    }
}

trait Dispatch: Send + Sync {
    fn start(
        &self,
        handle: AppServerRequestHandle,
    ) -> BoxFuture<'static, Result<UiReply, TypedRequestError>>;
    fn rejected(
        &self,
        app: &mut App,
        server: &mut AppServerSession,
        failure: &SubmissionFailure,
    ) -> Result<()>;
}

struct TypedRequest<F, T> {
    request: SyncMutex<Option<F>>,
    completion: Arc<Completion<T>>,
}

impl<F, Fut, T> Dispatch for TypedRequest<F, T>
where
    F: FnOnce(AppServerRequestHandle) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, TypedRequestError>> + Send + 'static,
    T: Send + 'static,
{
    fn start(
        &self,
        handle: AppServerRequestHandle,
    ) -> BoxFuture<'static, Result<UiReply, TypedRequestError>> {
        let request = self
            .request
            .lock()
            .expect("UI request owner poisoned")
            .take()
            .expect("UI request dispatched twice");
        let completion = Arc::clone(&self.completion);
        Box::pin(async move {
            tokio::select! {
                biased;
                _ = completion.cancelled.cancelled() => Ok(UiReply(SyncMutex::new(None))),
                result = request(handle) => {
                    let value = result?;
                    Ok(UiReply(SyncMutex::new(Some(Box::new(TypedReply { completion, value })))))
                }
            }
        })
    }

    fn rejected(
        &self,
        app: &mut App,
        server: &mut AppServerSession,
        failure: &SubmissionFailure,
    ) -> Result<()> {
        self.completion.finish(app, server, Err(failure))
    }
}

trait ApplyReply: Send {
    fn apply(self: Box<Self>, app: &mut App, server: &mut AppServerSession) -> Result<()>;
}

struct TypedReply<T> {
    completion: Arc<Completion<T>>,
    value: T,
}

impl<T: Send + 'static> ApplyReply for TypedReply<T> {
    fn apply(self: Box<Self>, app: &mut App, server: &mut AppServerSession) -> Result<()> {
        self.completion.finish(app, server, Ok(self.value))
    }
}

pub(in crate::app) struct UiReply(SyncMutex<Option<Box<dyn ApplyReply>>>);

impl std::fmt::Debug for UiReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("UiReply")
    }
}

impl UiReply {
    pub(super) fn apply(self, app: &mut App, server: &mut AppServerSession) -> Result<()> {
        match self.0.into_inner().expect("UI reply owner poisoned") {
            Some(reply) => reply.apply(app, server),
            None => Ok(()),
        }
    }
}

#[derive(Clone)]
pub(in crate::app) struct UiRequest {
    label: &'static str,
    cancelled: CancellationToken,
    kind: UiRequestKind,
    dispatch: Arc<dyn Dispatch>,
}

impl UiRequest {
    pub(super) fn label(&self) -> &'static str {
        self.label
    }

    pub(super) fn is_read_only(&self) -> bool {
        matches!(self.kind, UiRequestKind::ReadOnly)
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }

    pub(super) fn send(
        &self,
        handle: AppServerRequestHandle,
    ) -> BoxFuture<'static, Result<UiReply, TypedRequestError>> {
        self.dispatch.start(handle)
    }

    pub(super) fn rejected(
        &self,
        app: &mut App,
        server: &mut AppServerSession,
        failure: &SubmissionFailure,
    ) -> Result<()> {
        self.dispatch.rejected(app, server, failure)
    }
}

impl App {
    // Apply is synchronous: a continuation cannot await the server on the UI
    // task. Dependent requests re-enter this ledger instead.
    pub(in crate::app) fn request_server<T, F, Fut, A>(
        &mut self,
        server: &AppServerSession,
        scope: UiRequestScope,
        kind: UiRequestKind,
        label: &'static str,
        request: F,
        apply: A,
    ) -> UiRequestHandle
    where
        T: Send + 'static,
        F: FnOnce(AppServerRequestHandle) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, TypedRequestError>> + Send + 'static,
        A: FnOnce(&mut App, &mut AppServerSession, Result<T, &SubmissionFailure>) -> Result<()>
            + Send
            + 'static,
    {
        let cancelled = CancellationToken::new();
        let handle = UiRequestHandle(cancelled.clone());
        let request = UiRequest {
            label,
            cancelled: cancelled.clone(),
            kind,
            dispatch: Arc::new(TypedRequest {
                request: SyncMutex::new(Some(request)),
                completion: Arc::new(Completion {
                    scope,
                    cancelled,
                    label,
                    apply: SyncMutex::new(Some(Box::new(apply))),
                }),
            }),
        };
        match scope {
            UiRequestScope::Connection => self.enqueue_connection_operation(
                server,
                ServerOperation::Connection(ConnectionMutation::UiRequest(request)),
            ),
            UiRequestScope::Thread(thread_id) | UiRequestScope::DisplayedThread(thread_id) => {
                let operation = match kind {
                    UiRequestKind::ReadOnly => {
                        ServerOperation::Catalog(CatalogRequest::UiRequest(request))
                    }
                    UiRequestKind::Mutation => {
                        ServerOperation::Mutation(ThreadMutation::UiRequest(request))
                    }
                };
                self.enqueue_server_operation(server, thread_id, operation);
            }
        }
        handle
    }
}
