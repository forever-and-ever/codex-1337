//! Deferred ownership proofs for app-server notification routing.

use super::super::turn_submissions::ui_requests::UiRequestKind;
use super::super::turn_submissions::ui_requests::UiRequestScope;
use super::*;
use crate::app_event::AppEvent;
use crate::app_server_session::typed_requests::AppServerRequestHandleExt;
use codex_app_server_protocol::ThreadStatus;

impl App {
    pub(in crate::app) fn verify_untracked_mcp_notification(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        notification: ServerNotification,
    ) {
        let epoch = self.server_operations.epoch();
        self.request_server(
                server, UiRequestScope::Connection, UiRequestKind::ReadOnly, "Verifying MCP notification owner",
                move |handle| async move { Ok(read_mcp_subagent_parent_thread_id(handle, thread_id).await) },
                move |app, _, result| {
                    if let Ok(Some(parent_thread_id)) = result && app.is_family_thread(parent_thread_id) {
                        app.app_event_tx.send(AppEvent::VerifiedServerNotification(Box::new(VerifiedServerNotification {
                            epoch, notification, verification: Verification::Mcp { thread_id, parent_thread_id },
                        })));
                    }
                    Ok(())
                },
            );
    }

    pub(in crate::app) fn verify_thread_closure(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        notification: ServerNotification,
    ) {
        let epoch = self.server_operations.epoch();
        let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
        self.request_server(
            server,
            UiRequestScope::DisplayedThread(thread_id),
            UiRequestKind::ReadOnly,
            "Verifying displayed thread closure",
            move |handle| async move {
                let result = tokio::time::timeout(Duration::from_secs(5), async {
                    for attempt in 0..2 {
                        let result = handle
                            .thread_read(
                                RequestId::String(format!("thread-closed-{thread_id}-{attempt}")),
                                ThreadReadParams {
                                    thread_id: thread_id.to_string(),
                                    include_turns: false,
                                },
                            )
                            .await;
                        match result {
                            Ok(response) => {
                                return Ok(
                                    if matches!(response.thread.status, ThreadStatus::NotLoaded) {
                                        Closure::Closed
                                    } else {
                                        Closure::Loaded
                                    },
                                );
                            }
                            Err(TypedRequestError::Server { source, .. })
                                if matches!(source.code, -32602..=-32600) =>
                            {
                                return Ok(Closure::Closed);
                            }
                            Err(TypedRequestError::Server { source, .. })
                                if source.code == -32603 && attempt == 0 =>
                            {
                                continue;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    unreachable!("closure verification exhausted without a result")
                })
                .await;
                result.unwrap_or(Ok(Closure::Unconfirmed))
            },
            move |app, _, result| {
                match result {
                    Ok(Closure::Loaded) => return Ok(()),
                    Ok(Closure::Closed) => {}
                    Ok(Closure::Unconfirmed) | Err(_) => {
                        tracing::warn!("could not confirm displayed thread closure");
                        if app.begin_reconnect() {
                            return Ok(());
                        }
                    }
                }
                app.app_event_tx
                    .send(AppEvent::VerifiedServerNotification(Box::new(
                        VerifiedServerNotification {
                            epoch,
                            notification,
                            verification: Verification::Closed { thread_id, store },
                        },
                    )));
                Ok(())
            },
        );
    }

    pub(in crate::app) async fn finish_verified_server_notification(
        &mut self,
        server: &AppServerSession,
        verified: VerifiedServerNotification,
    ) {
        if verified.epoch != self.server_operations.epoch() {
            tracing::debug!("dropping verified notification from a retired connection");
            return;
        }
        match verified.verification {
            Verification::Closed { thread_id, store } => {
                if self.current_displayed_thread_id() != Some(thread_id)
                    || !self
                        .thread_event_channels
                        .get(&thread_id)
                        .is_some_and(|channel| Arc::ptr_eq(&channel.store, &store))
                {
                    tracing::debug!(%thread_id, "dropping stale verified close notification");
                    return;
                }
                self.route_verified_server_notification(server, verified.notification)
                    .await;
            }
            Verification::Mcp {
                thread_id,
                parent_thread_id,
            } => {
                if self.primary_thread_id.is_some()
                    && self.is_family_thread(parent_thread_id)
                    && let Err(error) = self
                        .enqueue_thread_notification(thread_id, verified.notification)
                        .await
                {
                    tracing::warn!(%error, "failed to enqueue verified MCP notification");
                }
            }
        }
    }
}

enum Verification {
    Closed {
        thread_id: ThreadId,
        store: Arc<tokio::sync::Mutex<super::super::thread_events::ThreadEventStore>>,
    },
    Mcp {
        thread_id: ThreadId,
        parent_thread_id: ThreadId,
    },
}

/// A routing proof revalidated again when its UI event is reduced.
pub(crate) struct VerifiedServerNotification {
    epoch: u64,
    notification: ServerNotification,
    verification: Verification,
}

impl std::fmt::Debug for VerifiedServerNotification {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedServerNotification")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

enum Closure {
    Loaded,
    Closed,
    Unconfirmed,
}
