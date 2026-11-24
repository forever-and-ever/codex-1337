//! A transcript click asks the originating server about the exact thread before desktop handoff.

use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_protocol::ThreadId;

use super::App;
use super::thread_events::ConnectionEpoch;
use super::turn_submissions::ui_requests::UiRequestKind;
use super::turn_submissions::ui_requests::UiRequestScope;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::backend_id::BackendId;
use crate::desktop_thread::DesktopRoute;
use crate::pager_overlay::Overlay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThreadLinkSource {
    thread_id: ThreadId,
    epoch: ConnectionEpoch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThreadLinkRequest {
    source: ThreadLinkSource,
    backend_id: BackendId,
    target: ThreadId,
}

impl App {
    pub(crate) fn capture_thread_link_source(&self) -> Option<ThreadLinkSource> {
        Some(ThreadLinkSource {
            thread_id: self.current_displayed_thread_id()?,
            epoch: self.reconnect.connection_epoch,
        })
    }

    pub(super) fn request_thread_link(
        &mut self,
        server: &AppServerSession,
        source_thread_id: Option<ThreadId>,
        target: ThreadId,
    ) {
        let source = source_thread_id.map(|thread_id| ThreadLinkSource {
            thread_id,
            epoch: self.reconnect.connection_epoch,
        });
        self.request_thread_link_from_source(server, source, target);
    }

    pub(crate) fn request_thread_link_from_source(
        &mut self,
        server: &AppServerSession,
        source: Option<ThreadLinkSource>,
        target: ThreadId,
    ) {
        let Some(source) = source.filter(|source| source.epoch == self.reconnect.connection_epoch)
        else {
            self.show_thread_link_error(
                source,
                "This transcript is no longer connected to its app server.".into(),
            );
            return;
        };
        self.app_event_tx
            .send(AppEvent::OpenThreadLink(ThreadLinkRequest {
                source,
                backend_id: server.backend_id(),
                target,
            }));
    }

    fn thread_link_backend_is_current(
        &self,
        server: &AppServerSession,
        request: ThreadLinkRequest,
    ) -> bool {
        let channel = self.thread_event_channels.get(&request.source.thread_id);
        !self.reconnect.offline
            && request.source.epoch == self.reconnect.connection_epoch
            && request.backend_id == server.backend_id()
            && (channel.is_some()
                || self.current_displayed_thread_id() == Some(request.source.thread_id))
            && channel
                .and_then(|channel| channel.backend_id)
                .is_none_or(|source_owner| source_owner == request.backend_id)
    }

    pub(super) fn open_thread_link(
        &mut self,
        server: &AppServerSession,
        request: ThreadLinkRequest,
    ) {
        if !self.thread_link_backend_is_current(server, request) {
            self.show_thread_link_error(
                Some(request.source),
                "This transcript is no longer connected to its app server.".into(),
            );
            return;
        }

        if self.pending_thread_links.contains_key(&request.target) {
            return;
        }
        self.pending_thread_links.insert(request.target, request);
        let route = DesktopRoute::from_target(
            &self.app_server_target,
            server.uses_embedded_app_server(),
            self.config.codex_home.as_path(),
        );
        let target = request.target.to_string();
        self.request_server(
            server,
            UiRequestScope::Thread(request.source.thread_id),
            UiRequestKind::ReadOnly,
            "Check linked conversation",
            move |handle| async move {
                handle
                    .request_typed::<ThreadReadResponse>(ClientRequest::ThreadRead {
                        request_id: RequestId::String(uuid::Uuid::new_v4().to_string()),
                        params: ThreadReadParams {
                            thread_id: target,
                            include_turns: false,
                        },
                    })
                    .await
            },
            move |app, server, result| {
                if !app.thread_link_backend_is_current(server, request) {
                    app.finish_thread_link(server, request, Ok(()));
                    return Ok(());
                }
                match result {
                    Ok(response) if response.thread.id != request.target.to_string() => {
                        app.finish_thread_link(
                            server,
                            request,
                            Err(
                                "The linked conversation is not available on this app server."
                                    .into(),
                            ),
                        );
                    }
                    Ok(response) if response.thread.ephemeral => {
                        app.finish_thread_link(
                            server,
                            request,
                            Err("This linked conversation has no saved transcript.".into()),
                        );
                    }
                    Ok(_) => {
                        let app_event_tx = app.app_event_tx.clone();
                        tokio::spawn(async move {
                            let result =
                                route.open_or_focus(request.target).await.map_err(|error| {
                                    format!("Could not open the linked conversation: {error}")
                                });
                            app_event_tx
                                .send(AppEvent::ThreadLinkOpenCompleted { request, result });
                        });
                    }
                    Err(_) => app.finish_thread_link(
                        server,
                        request,
                        Err("The linked conversation is not available on this app server.".into()),
                    ),
                }
                Ok(())
            },
        );
    }

    pub(super) fn finish_thread_link(
        &mut self,
        server: &AppServerSession,
        request: ThreadLinkRequest,
        result: Result<(), String>,
    ) {
        if self.pending_thread_links.get(&request.target) != Some(&request) {
            return;
        }
        self.pending_thread_links.remove(&request.target);
        if let Err(message) = result
            && self.thread_link_backend_is_current(server, request)
        {
            self.show_thread_link_error(Some(request.source), message);
        }
    }

    fn show_thread_link_error(&mut self, source: Option<ThreadLinkSource>, message: String) {
        if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut()
            && overlay.thread_link_source == source
        {
            overlay.set_notice(message);
        } else {
            self.chat_widget.add_error_message(message);
        }
    }
}

#[cfg(test)]
#[path = "thread_link_handoff_tests.rs"]
mod tests;
