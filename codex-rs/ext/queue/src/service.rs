use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::Weak;
use std::time::Duration;

use codex_core::CodexThread;
use codex_core::NotSubmittedReason;
use codex_core::StartIfIdleSubmission;
use codex_core::ThreadManager;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::TurnStartOptions;
use codex_extension_api::ExtensionEventSink;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ThreadIdleCause;
use codex_extension_api::ThreadIdleInput;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadResumeInput;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadQueueChangedEvent;
use codex_protocol::protocol::W3cTraceContext;
use codex_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use codex_protocol::user_input::UserInput;
use codex_thread_store::MAX_QUEUE_ITEMS;
use codex_thread_store::QueueStore;
use codex_thread_store::QueuedUserSubmissionRecord;
use codex_thread_store::ThreadStoreError;
use codex_user_input::snapshot_local_user_input;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::sync::OwnedMutexGuard;
use tokio::sync::broadcast::error::TryRecvError;
use uuid::Uuid;

/// One user message or unsolicited tool output waiting to start on its thread.
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedItem {
    pub id: String,
    pub input: TurnInput,
    pub responsesapi_client_metadata: Option<HashMap<String, String>>,
}

#[derive(Debug, Error)]
pub enum QueueServiceError {
    #[error("queue storage failed: {0}")]
    Storage(#[from] ThreadStoreError),
    #[error("queued submission payload is invalid: {0}")]
    InvalidPayload(#[from] serde_json::Error),
    #[error("local queued attachment is invalid: {0}")]
    InvalidAttachment(#[from] std::io::Error),
    #[error("Core failed to submit queued message: {0}")]
    CoreSubmissionError(#[from] CodexErr),
    #[error("the queue accepts user input or named unsolicited tool output")]
    InvalidInput,
    #[error("queued tool messages can be removed but not edited")]
    ToolOutputReadOnly,
    #[error(
        "queued input exceeds the maximum length of {MAX_USER_INPUT_TEXT_CHARS} characters ({actual_chars} provided)"
    )]
    InputTooLarge { actual_chars: usize },
}

type PendingDispatches = Arc<StdMutex<HashSet<(ThreadId, String)>>>;

struct DispatchPin {
    key: (ThreadId, String),
    dispatches: PendingDispatches,
}

impl Drop for DispatchPin {
    fn drop(&mut self) {
        self.dispatches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

enum QueuedItemDispatch {
    Routed(StartIfIdleSubmission),
    Reconciled(String),
}

#[derive(Clone)]
pub struct QueuedItemService {
    queue: Arc<dyn QueueStore>,
    pending_dispatches: PendingDispatches,
    thread_manager: Weak<ThreadManager>,
    event_sink: Arc<dyn ExtensionEventSink>,
    dispatch_locks: Arc<StdMutex<HashMap<ThreadId, Weak<Mutex<()>>>>>,
    resumed_threads: Arc<StdMutex<HashSet<ThreadId>>>,
}

impl QueuedItemService {
    pub fn new(
        queue: Arc<dyn QueueStore>,
        thread_manager: Weak<ThreadManager>,
        event_sink: Arc<dyn ExtensionEventSink>,
    ) -> Self {
        Self {
            queue,
            pending_dispatches: Arc::default(),
            thread_manager,
            event_sink,
            dispatch_locks: Arc::new(StdMutex::new(HashMap::new())),
            resumed_threads: Arc::new(StdMutex::new(HashSet::new())),
        }
    }

    // Check SQLite's inexpensive data version every 10 seconds, then use the
    // durable revision index to discover only changed threads. Independent
    // dispatch tasks keep a blocked or failed thread from starving other queues.
    pub(crate) async fn watch_external_messages(service: Weak<Self>) {
        let mut last_version = None;
        let mut last_revision = 0;
        let mut dispatches: HashMap<ThreadId, tokio::task::JoinHandle<()>> = HashMap::new();
        let mut interval = tokio::time::interval(Duration::from_secs(/*secs*/ 10));
        let mut manager_initialized = false;
        let mut thread_created = None;
        let mut newly_loaded_threads = HashSet::new();
        loop {
            interval.tick().await;
            let Some(service) = service.upgrade() else {
                return;
            };
            let Some(manager) = service.thread_manager.upgrade() else {
                if manager_initialized {
                    return;
                }
                drop(service);
                tokio::time::sleep(Duration::from_millis(/*millis*/ 1)).await;
                interval.reset_immediately();
                continue;
            };
            manager_initialized = true;
            let thread_created =
                thread_created.get_or_insert_with(|| manager.subscribe_thread_created());
            loop {
                match thread_created.try_recv() {
                    Ok(thread_id) => {
                        newly_loaded_threads.insert(thread_id);
                    }
                    Err(TryRecvError::Lagged(_)) => {
                        newly_loaded_threads.extend(manager.list_thread_ids().await);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Closed) => return,
                }
            }
            newly_loaded_threads.extend(
                service
                    .resumed_threads
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .drain(),
            );

            let version = match service.queue.change_version().await {
                Ok(version) => version,
                Err(error) => {
                    tracing::warn!(%error, "failed to check queue change version");
                    continue;
                }
            };
            let version_changed = last_version != Some(version);
            if !version_changed && newly_loaded_threads.is_empty() {
                continue;
            }

            let thread_ids = manager.list_thread_ids().await;
            let mut changes = Vec::new();
            let mut observed_revision = last_revision;
            if version_changed {
                match service
                    .queue
                    .changes_since(last_revision, &thread_ids)
                    .await
                {
                    Ok(changed_threads) => {
                        if let Some((_, revision)) = changed_threads.last() {
                            observed_revision = *revision;
                        }
                        changes.extend(changed_threads);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to discover changed thread queues");
                        continue;
                    }
                }
            }
            if !newly_loaded_threads.is_empty() {
                let created_threads = thread_ids
                    .iter()
                    .copied()
                    .filter(|thread_id| newly_loaded_threads.contains(thread_id))
                    .collect::<Vec<_>>();
                match service
                    .queue
                    .changes_since(/*revision*/ 0, &created_threads)
                    .await
                {
                    Ok(changed_threads) => changes.extend(changed_threads),
                    Err(error) => {
                        tracing::warn!(%error, "failed to discover newly loaded thread queues");
                        continue;
                    }
                }
            }
            last_version = Some(version);
            last_revision = observed_revision;
            newly_loaded_threads.clear();
            dispatches.retain(|_, dispatch| !dispatch.is_finished());

            let mut changed_threads = HashSet::new();
            for (thread_id, _) in changes {
                if !changed_threads.insert(thread_id) {
                    continue;
                }
                service.emit_changed(thread_id);
                if dispatches
                    .get(&thread_id)
                    .is_some_and(|dispatch| !dispatch.is_finished())
                {
                    continue;
                }
                let service = Arc::downgrade(&service);
                let dispatch = tokio::spawn(async move {
                    loop {
                        {
                            let Some(service) = service.upgrade() else {
                                return;
                            };
                            let Some(manager) = service.thread_manager.upgrade() else {
                                return;
                            };
                            let Ok(thread) = manager.get_thread(thread_id).await else {
                                return;
                            };
                            if matches!(
                                thread.agent_status().await,
                                AgentStatus::Running
                                    | AgentStatus::Interrupted
                                    | AgentStatus::Shutdown
                                    | AgentStatus::NotFound
                            ) {
                                return;
                            }
                            match service
                                .queue
                                .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
                                .await
                            {
                                Ok(items) if items.is_empty() => return,
                                Ok(_) => service.wake_if_loaded(thread_id).await,
                                Err(error) => {
                                    tracing::warn!(%thread_id, %error, "failed to check queued user input");
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(/*secs*/ 10)).await;
                    }
                });
                dispatches.insert(thread_id, dispatch);
            }
        }
    }

    fn dispatch_lock(&self, thread_id: ThreadId) -> Arc<Mutex<()>> {
        let mut locks = self
            .dispatch_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() != 0);
        if let Some(lock) = locks.get(&thread_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(thread_id, Arc::downgrade(&lock));
        lock
    }

    async fn dispatch_guard(&self, thread_id: ThreadId) -> OwnedMutexGuard<()> {
        self.dispatch_lock(thread_id).lock_owned().await
    }

    pub async fn enqueue(
        &self,
        thread_id: ThreadId,
        input: TurnInput,
    ) -> Result<QueuedItem, QueueServiceError> {
        self.enqueue_with_metadata(thread_id, input, /*responsesapi_client_metadata*/ None)
            .await
    }

    pub async fn enqueue_with_metadata(
        &self,
        thread_id: ThreadId,
        input: TurnInput,
        responsesapi_client_metadata: Option<HashMap<String, String>>,
    ) -> Result<QueuedItem, QueueServiceError> {
        let input = prepare_queued_input(input).await?;
        let payload = queued_input_payload(&input, responsesapi_client_metadata.as_ref())?;
        let item = {
            let _dispatch_guard = self.dispatch_guard(thread_id).await;
            let item = QueuedItem::try_from(self.queue.enqueue(thread_id, payload).await?)?;
            self.emit_changed(thread_id);
            item
        };
        self.wake_if_loaded(thread_id).await;
        Ok(item)
    }

    pub async fn list(&self, thread_id: ThreadId) -> Result<Vec<QueuedItem>, QueueServiceError> {
        self.list_page(thread_id, /*offset*/ 0, MAX_QUEUE_ITEMS)
            .await
    }

    pub async fn list_page(
        &self,
        thread_id: ThreadId,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<QueuedItem>, QueueServiceError> {
        self.queue
            .list_page(thread_id, offset, limit)
            .await?
            .into_iter()
            .map(QueuedItem::try_from)
            .collect()
    }

    pub async fn update(
        &self,
        thread_id: ThreadId,
        queued_item_id: String,
        input: TurnInput,
    ) -> Result<Option<QueuedItem>, QueueServiceError> {
        self.update_with_metadata(
            thread_id,
            queued_item_id,
            input,
            /*responsesapi_client_metadata*/ None,
        )
        .await
    }

    /// Omitted or empty metadata preserves the input's attribution, not the current controller's.
    pub async fn update_with_metadata(
        &self,
        thread_id: ThreadId,
        queued_item_id: String,
        mut input: TurnInput,
        mut responsesapi_client_metadata: Option<HashMap<String, String>>,
    ) -> Result<Option<QueuedItem>, QueueServiceError> {
        responsesapi_client_metadata =
            responsesapi_client_metadata.filter(|metadata| !metadata.is_empty());
        let _dispatch_guard = self.dispatch_guard(thread_id).await;
        self.check_not_dispatched(thread_id, &queued_item_id)?;
        if let Some(manager) = self.thread_manager.upgrade()
            && let Ok(thread) = manager.get_thread(thread_id).await
            && thread
                .queued_input_turn_id(&queued_item_id)
                .await?
                .is_some()
        {
            return Err(ThreadStoreError::InvalidRequest {
                message: format!("queued submission was already accepted: {queued_item_id}"),
            }
            .into());
        }
        let previous = self
            .list(thread_id)
            .await?
            .into_iter()
            .find(|item| item.id == queued_item_id);
        if let TurnInput::UserInput { client_id, .. } = &mut input {
            *client_id = match previous.as_ref().map(|item| &item.input) {
                Some(TurnInput::UserInput { client_id, .. }) => client_id.clone(),
                Some(_) => return Err(QueueServiceError::ToolOutputReadOnly),
                None => None,
            };
        } else {
            return Err(QueueServiceError::ToolOutputReadOnly);
        }
        if responsesapi_client_metadata.is_none() {
            responsesapi_client_metadata =
                previous.and_then(|item| item.responsesapi_client_metadata);
        }
        let input = prepare_queued_input(input).await?;
        let payload = queued_input_payload(&input, responsesapi_client_metadata.as_ref())?;
        let item = self
            .queue
            .update(thread_id, queued_item_id, payload)
            .await?
            .map(QueuedItem::try_from)
            .transpose()?;
        if item.is_some() {
            self.emit_changed(thread_id);
        }
        Ok(item)
    }

    pub async fn delete(
        &self,
        thread_id: ThreadId,
        queued_item_id: String,
    ) -> Result<bool, QueueServiceError> {
        let _dispatch_guard = self.dispatch_guard(thread_id).await;
        self.check_not_dispatched(thread_id, &queued_item_id)?;
        self.delete_locked(thread_id, queued_item_id).await
    }

    async fn delete_locked(
        &self,
        thread_id: ThreadId,
        queued_item_id: String,
    ) -> Result<bool, QueueServiceError> {
        let deleted = self.queue.delete(thread_id, queued_item_id).await?;
        if deleted {
            self.emit_changed(thread_id);
        }
        Ok(deleted)
    }

    pub async fn reorder(
        &self,
        thread_id: ThreadId,
        ordered_ids: Vec<String>,
    ) -> Result<(), QueueServiceError> {
        let _dispatch_guard = self.dispatch_guard(thread_id).await;
        self.queue.reorder(thread_id, ordered_ids).await?;
        self.emit_changed(thread_id);
        Ok(())
    }

    fn is_dispatched(&self, thread_id: ThreadId, id: &str) -> bool {
        self.pending_dispatches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&(thread_id, id.to_owned()))
    }

    fn check_not_dispatched(&self, thread_id: ThreadId, id: &str) -> Result<(), QueueServiceError> {
        if self.is_dispatched(thread_id, id) {
            return Err(ThreadStoreError::InvalidRequest {
                message: format!("queued submission is being dispatched: {id}"),
            }
            .into());
        }
        Ok(())
    }

    async fn route(
        &self,
        thread: &CodexThread,
        item: QueuedItem,
        trace: Option<W3cTraceContext>,
    ) -> Result<QueuedItemDispatch, QueueServiceError> {
        let thread_id = thread.startup_metadata().thread_id;
        let QueuedItem {
            id,
            input,
            responsesapi_client_metadata,
        } = item;
        if let Some(turn_id) = thread.queued_input_turn_id(&id).await? {
            self.delete_locked(thread_id, id).await?;
            return Ok(QueuedItemDispatch::Reconciled(turn_id));
        }
        let key = (thread_id, id.clone());
        if !self
            .pending_dispatches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone())
        {
            return Ok(QueuedItemDispatch::Routed(
                StartIfIdleSubmission::NotSubmitted {
                    reason: NotSubmittedReason::NotIdle,
                },
            ));
        }
        let pin = DispatchPin {
            key,
            dispatches: self.pending_dispatches.clone(),
        };
        let service = self.clone();
        let cleanup_id = id.clone();
        let submission = thread.start_queued_input_if_idle(
                TurnInputRequest::new(input)
                    .with_responses_metadata(responsesapi_client_metadata)
                    .with_trace(trace)
                    .on_start(TurnStartOptions {
                        turn_trigger: Some("queue".to_owned()),
                        ..Default::default()
                    }),
                id.clone(),
                move || async move {
                    let _guard = service.dispatch_guard(thread_id).await;
                    if let Err(error) = service.delete_locked(thread_id, cleanup_id.clone()).await {
                        tracing::warn!(%thread_id, id = %cleanup_id, %error, "failed to clean up accepted queued input");
                    }
                    drop(pin);
                },
            ).await?;
        if matches!(&submission, StartIfIdleSubmission::Started { .. })
            && !self.is_dispatched(thread_id, &id)
            && let Some(turn_id) = thread.queued_input_turn_id(&id).await?
        {
            self.delete_locked(thread_id, id).await?;
            return Ok(QueuedItemDispatch::Reconciled(turn_id));
        }
        Ok(QueuedItemDispatch::Routed(submission))
    }

    /// Starts the selected queued message only when its thread is idle.
    pub async fn start(
        &self,
        thread: &CodexThread,
        queued_item_id: Option<String>,
        trace: Option<W3cTraceContext>,
    ) -> Result<StartIfIdleSubmission, QueueServiceError> {
        let thread_id = thread.startup_metadata().thread_id;
        let _dispatch_guard = self.dispatch_guard(thread_id).await;
        let item = self
            .list(thread_id)
            .await?
            .into_iter()
            .find(|item| queued_item_id.as_ref().is_none_or(|id| item.id == *id))
            .ok_or_else(|| ThreadStoreError::InvalidRequest {
                message: queued_item_id.as_ref().map_or_else(
                    || "queue is empty".to_string(),
                    |id| format!("queued submission not found: {id}"),
                ),
            })?;
        if !supported_queue_input(&item.input) {
            return Err(QueueServiceError::InvalidInput);
        }
        Ok(match self.route(thread, item, trace).await? {
            QueuedItemDispatch::Routed(submission) => submission,
            QueuedItemDispatch::Reconciled(turn_id) => StartIfIdleSubmission::Started {
                root_turn_id: turn_id.clone(),
                turn_id,
            },
        })
    }

    async fn dispatch_if_idle(&self, thread_id: ThreadId) -> Result<(), QueueServiceError> {
        let Some(manager) = self.thread_manager.upgrade() else {
            return Ok(());
        };
        let Ok(thread) = manager.get_thread(thread_id).await else {
            return Ok(());
        };

        loop {
            let Some(record) = self
                .queue
                .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
                .await?
                .into_iter()
                .next()
            else {
                return Ok(());
            };
            let queued_item_id = record.id.clone();

            let item = match QueuedItem::try_from(record) {
                Ok(item) => item,
                Err(error) => {
                    tracing::warn!(%queued_item_id, %error, "discarding invalid queued item");
                    self.delete_locked(thread_id, queued_item_id).await?;
                    continue;
                }
            };
            if !supported_queue_input(&item.input) {
                tracing::warn!(%queued_item_id, "discarding unsupported queued input");
                self.delete_locked(thread_id, queued_item_id).await?;
                continue;
            }

            match self.route(&thread, item, /*trace*/ None).await {
                Ok(QueuedItemDispatch::Reconciled(_)) => continue,
                Ok(QueuedItemDispatch::Routed(StartIfIdleSubmission::Started { .. })) => {
                    return Ok(());
                }
                Ok(QueuedItemDispatch::Routed(StartIfIdleSubmission::NotSubmitted { reason })) => {
                    tracing::debug!(%thread_id, %queued_item_id, ?reason, "core deferred queued input");
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(%thread_id, %queued_item_id, %error, "core could not start queued input");
                    return Ok(());
                }
            }
        }
    }

    async fn wake_if_loaded(&self, thread_id: ThreadId) {
        let Some(manager) = self.thread_manager.upgrade() else {
            return;
        };
        if let Ok(thread) = manager.get_thread(thread_id).await
            && !matches!(thread.agent_status().await, AgentStatus::Interrupted)
        {
            thread
                .emit_thread_idle_lifecycle_if_idle(ThreadIdleCause::Completed)
                .await;
        }
    }

    fn emit_changed(&self, thread_id: ThreadId) {
        self.event_sink.emit(Event {
            id: Uuid::now_v7().to_string(),
            msg: EventMsg::ThreadQueueChanged(ThreadQueueChangedEvent { thread_id }),
        });
    }
}

async fn prepare_queued_input(mut input: TurnInput) -> Result<TurnInput, QueueServiceError> {
    if let TurnInput::ResponseItem(ResponseItem::FunctionCallOutput { output, .. }) = &input {
        if !supported_queue_input(&input) {
            return Err(QueueServiceError::InvalidInput);
        }
        let actual_chars = output.body.to_text().map_or(0, |text| text.chars().count());
        if actual_chars > MAX_USER_INPUT_TEXT_CHARS {
            return Err(QueueServiceError::InputTooLarge { actual_chars });
        }
        return Ok(input);
    }
    let TurnInput::UserInput { content, client_id } = &mut input else {
        return Err(QueueServiceError::InvalidInput);
    };
    if content.is_empty() {
        return Err(QueueServiceError::InvalidInput);
    }
    let actual_chars: usize = content
        .iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text.chars().count()),
            _ => None,
        })
        .sum();
    if actual_chars > MAX_USER_INPUT_TEXT_CHARS {
        return Err(QueueServiceError::InputTooLarge { actual_chars });
    }
    client_id.get_or_insert_with(|| Uuid::now_v7().to_string());
    if !content.iter().any(|item| {
        matches!(
            item,
            UserInput::LocalImage { .. } | UserInput::LocalAudio { .. }
        )
    }) {
        return Ok(input);
    }

    tokio::task::spawn_blocking(move || {
        let mut input = input;
        if let TurnInput::UserInput { content, .. } = &mut input {
            for item in content {
                snapshot_local_user_input(item)?;
            }
        }
        Ok::<TurnInput, std::io::Error>(input)
    })
    .await
    .map_err(|error| QueueServiceError::InvalidAttachment(std::io::Error::other(error)))?
    .map_err(QueueServiceError::InvalidAttachment)
}

fn supported_queue_input(input: &TurnInput) -> bool {
    // TELOS(server-hosted-agents): attributed deliveries stay tool output in the durable queue.
    let item = match input {
        TurnInput::UserInput { content, .. } => return !content.is_empty(),
        TurnInput::ResponseItem(item) => item,
        TurnInput::AnnotatedResponseItem(annotated) => &annotated.item,
        TurnInput::InterAgentCommunication(_) => return false,
    };
    matches!(item, ResponseItem::FunctionCallOutput {
        call_id: None,
        name: Some(name),
        ..
    } if !name.trim().is_empty())
}

impl<C> ThreadLifecycleContributor<C> for QueuedItemService
where
    C: Send + Sync + 'static,
{
    fn on_thread_resume<'a>(&'a self, input: ThreadResumeInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) {
                self.resumed_threads
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(thread_id);
            }
        })
    }

    fn on_thread_idle<'a>(&'a self, input: ThreadIdleInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if input.cause == ThreadIdleCause::Interrupted {
                return;
            }
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                tracing::warn!(
                    level_id = input.thread_store.level_id(),
                    "queue extension received an invalid thread id"
                );
                return;
            };
            let _guard = self.dispatch_guard(thread_id).await;
            if let Err(error) = self.dispatch_if_idle(thread_id).await {
                tracing::warn!(%thread_id, %error, "failed to dispatch queued input");
            }
        })
    }
}

fn queued_input_payload(
    input: &TurnInput,
    metadata: Option<&HashMap<String, String>>,
) -> Result<String, QueueServiceError> {
    let Some(metadata) = metadata else {
        return Ok(serde_json::to_string(input)?);
    };
    let mut payload = serde_json::to_value(input)?;
    // Keep TurnInput's wire shape so older readers can still execute the input.
    payload["UserInput"]["responsesapi_client_metadata"] = serde_json::to_value(metadata)?;
    Ok(serde_json::to_string(&payload)?)
}

impl TryFrom<QueuedUserSubmissionRecord> for QueuedItem {
    type Error = QueueServiceError;

    fn try_from(record: QueuedUserSubmissionRecord) -> Result<Self, Self::Error> {
        let mut payload: serde_json::Value = serde_json::from_str(&record.payload)?;
        let responsesapi_client_metadata = payload
            .get_mut("UserInput")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|input| input.remove("responsesapi_client_metadata"))
            .map(serde_json::from_value)
            .transpose()?;
        Ok(QueuedItem {
            id: record.id,
            input: serde_json::from_value(payload)?,
            responsesapi_client_metadata,
        })
    }
}
