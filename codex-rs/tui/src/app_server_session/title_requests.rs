//! TELOS(fast-ui-thread): title I/O is owned by continuations; manual intent wins every race.

use super::AppServerSession;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadSetNameParams;
use codex_app_server_protocol::ThreadSetNameResponse;
use codex_protocol::ThreadId;
use futures::lock::Mutex;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Serializes one thread's name writes across the RPC; epochs, not waiter order,
/// settle automatic versus manual intent.
pub(super) struct NameGate {
    serial: Mutex<()>,
    manual_epoch: AtomicU64,
}

pub(crate) struct AutomaticThreadName {
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    gate: Arc<NameGate>,
    epoch: u64,
}

#[derive(Clone)]
pub(crate) struct ManualThreadName {
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    gate: Arc<NameGate>,
}

impl AppServerSession {
    fn name_gate(&self, thread_id: ThreadId) -> Arc<NameGate> {
        let mut gates = self
            .automatic_name_writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates.retain(|_, gate| Arc::strong_count(gate) > 1);
        Arc::clone(gates.entry(thread_id).or_insert_with(|| {
            Arc::new(NameGate {
                serial: Mutex::new(()),
                manual_epoch: AtomicU64::new(0),
            })
        }))
    }

    pub(crate) fn automatic_thread_name_writer(&self, thread_id: ThreadId) -> AutomaticThreadName {
        let gate = self.name_gate(thread_id);
        let epoch = gate.manual_epoch.load(Ordering::Acquire);
        AutomaticThreadName {
            handle: self.request_handle(),
            thread_id,
            gate,
            epoch,
        }
    }

    pub(crate) fn manual_thread_name_writer(&self, thread_id: ThreadId) -> ManualThreadName {
        let gate = self.name_gate(thread_id);
        gate.manual_epoch.fetch_add(1, Ordering::AcqRel);
        ManualThreadName {
            handle: self.request_handle(),
            thread_id,
            gate,
        }
    }

    #[cfg(test)]
    pub(crate) async fn thread_set_name(
        &mut self,
        thread_id: ThreadId,
        name: String,
    ) -> color_eyre::Result<()> {
        let writer = self.manual_thread_name_writer(thread_id);
        let request_id = self.next_request_id();
        writer
            .save(
                request_id,
                ThreadSetNameParams {
                    thread_id: thread_id.to_string(),
                    name,
                    naming_mode: Some(codex_app_server_protocol::ThreadNameMode::Explicit),
                    conflict_thread_ids: None,
                },
            )
            .await?;
        Ok(())
    }
}

impl AutomaticThreadName {
    pub(crate) async fn save(self, title: String, cancellation: &CancellationToken) {
        let read = self
            .handle
            .request_typed::<ThreadReadResponse>(ClientRequest::ThreadRead {
                request_id: RequestId::String(format!("title-read-{}", Uuid::new_v4())),
                params: ThreadReadParams {
                    thread_id: self.thread_id.to_string(),
                    include_turns: false,
                },
            });
        let Some(Ok(response)) = cancellation.run_until_cancelled(read).await else {
            return;
        };
        if response.thread.name.is_some() {
            return;
        }
        let Some(_permit) = cancellation
            .run_until_cancelled(self.gate.serial.lock())
            .await
        else {
            return;
        };
        if self.gate.manual_epoch.load(Ordering::Acquire) != self.epoch
            || cancellation.is_cancelled()
        {
            return;
        }
        if let Err(error) = self
            .handle
            .request_typed::<ThreadSetNameResponse>(ClientRequest::ThreadSetName {
                request_id: RequestId::String(format!("title-save-{}", Uuid::new_v4())),
                params: ThreadSetNameParams {
                    thread_id: self.thread_id.to_string(),
                    name: title,
                    naming_mode: Some(codex_app_server_protocol::ThreadNameMode::Generated),
                    conflict_thread_ids: None,
                },
            })
            .await
        {
            tracing::debug!(%error, "failed to apply generated thread title");
        }
    }
}

impl ManualThreadName {
    pub(crate) async fn save(
        self,
        request_id: RequestId,
        params: ThreadSetNameParams,
    ) -> Result<ThreadSetNameResponse, TypedRequestError> {
        assert_eq!(
            params.thread_id,
            self.thread_id.to_string(),
            "manual name request changed its captured thread"
        );
        let _permit = self.gate.serial.lock().await;
        self.handle
            .request_typed(ClientRequest::ThreadSetName { request_id, params })
            .await
    }
}
