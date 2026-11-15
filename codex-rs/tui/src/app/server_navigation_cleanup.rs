use super::ACK_TIMEOUT;
use super::AbortOnDropHandle;
use super::App;
use super::AppServerSession;
use super::ClientRequest;
use super::FutureExt;
use super::LaneState;
use super::NavigationCompletion;
use super::NavigationIntent;
use super::NavigationReceipt;
use super::PendingNavigation;
use super::Result;
use super::ServerCompletion;
use super::ThreadId;
use super::Uuid;
use super::detach;
use super::selection;
use codex_app_server_client::AppServerRequestHandle;

pub(super) enum CleanupContinuation {
    Done,
    ArchiveFork,
    SelectionFallback(Box<selection::SelectionContext>),
}

impl App {
    pub(in crate::app) fn adopt_navigation_cleanup(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
    ) {
        self.adopt_navigation_cleanup_on(server.request_handle(), thread_id);
    }

    #[expect(
        clippy::expect_used,
        reason = "The operation sequence cannot exhaust u64 within one frontend lifetime."
    )]
    pub(in crate::app) fn adopt_navigation_cleanup_on(
        &mut self,
        handle: AppServerRequestHandle,
        thread_id: ThreadId,
    ) {
        let order = self.server_operations.next_order;
        self.server_operations.next_order =
            order.checked_add(1).expect("operation sequence exhausted");
        let operation = PendingNavigation {
            id: Uuid::new_v4(),
            order,
            epoch: self.server_operations.epoch,
            generation: self.server_operations.navigation.generation,
            intent: NavigationIntent::Transitioning,
            state: LaneState::Queued,
        };
        self.start_navigation_cleanup(
            operation,
            handle,
            thread_id,
            Vec::new(),
            CleanupContinuation::Done,
            false,
        );
        self.refresh_submission_preview();
    }

    pub(super) fn start_navigation_cleanup(
        &mut self,
        mut operation: PendingNavigation,
        handle: AppServerRequestHandle,
        thread_id: ThreadId,
        fences: Vec<ThreadId>,
        next: CleanupContinuation,
        matching: bool,
    ) {
        let archive = matches!(next, CleanupContinuation::ArchiveFork);
        operation.intent = NavigationIntent::Cleanup {
            thread_id,
            fences,
            next,
        };
        let completions = self.server_operations.control_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async {
                tokio::time::timeout(ACK_TIMEOUT, detach::unsubscribe(&handle, id, thread_id))
                    .await
                    .map_err(|_| {
                        "Unsubscribe acknowledgement timed out; it may still complete".to_string()
                    })?
                    .map_err(|error| error.to_string())?;
                if archive {
                    tokio::time::timeout(
                        ACK_TIMEOUT,
                        handle.request_typed::<codex_app_server_protocol::ThreadArchiveResponse>(
                            ClientRequest::ThreadArchive {
                                request_id: codex_app_server_protocol::RequestId::String(format!(
                                    "tui-cleanup-archive-{id}"
                                )),
                                params: codex_app_server_protocol::ThreadArchiveParams {
                                    thread_id: thread_id.to_string(),
                                },
                            },
                        ),
                    )
                    .await
                    .map_err(|_| {
                        "Replacement fork archive acknowledgement timed out; it may still complete"
                            .to_string()
                    })?
                    .map_err(|error| error.to_string())?;
                }
                Ok(())
            })
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err("Unsubscribe task terminated unexpectedly".into()));
            let _ = completions.send(ServerCompletion::Navigation(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::Cleaned(result),
                },
            )));
        })));
        if matching {
            self.server_operations.navigation.current = Some(operation);
        } else {
            self.server_operations.navigation.retired.push(operation);
        }
    }

    pub(super) fn retain_navigation_uncertainty(
        &mut self,
        mut operation: PendingNavigation,
        fences: Vec<ThreadId>,
        matching: bool,
    ) {
        operation.intent = NavigationIntent::Uncertain(fences);
        operation.state = LaneState::Uncertain;
        if matching {
            self.server_operations.navigation.current = Some(operation);
        } else {
            self.server_operations.navigation.retired.push(operation);
        }
    }

    pub(super) fn finish_navigation_cleanup(
        &mut self,
        server: &AppServerSession,
        mut operation: PendingNavigation,
        matching: bool,
        result: Result<(), String>,
    ) {
        let NavigationIntent::Cleanup {
            thread_id,
            mut fences,
            next,
        } = std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("cleanup receipt without subscription custody");
        };
        if operation.epoch != self.server_operations.epoch {
            return;
        }
        if let Err(error) = result {
            self.chat_widget.add_error_message(format!("Detaching superseded conversation {thread_id} was not confirmed: {error}. Close and reopen this TUI to resynchronize; no detach was retried."));
            fences.push(thread_id);
        }
        if !fences.is_empty() {
            self.retain_navigation_uncertainty(operation, fences, matching);
        } else if matching && let CleanupContinuation::SelectionFallback(context) = next {
            self.start_selection_fallback(server, operation, context);
        }
    }
}
