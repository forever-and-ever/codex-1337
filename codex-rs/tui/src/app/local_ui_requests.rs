//! Local asynchronous work returns through the same typed UI reply as server requests.

use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::Result;
use super::turn_submissions::ui_requests::UiReply;
#[cfg(test)]
use crate::tui;
use std::future::Future;
#[cfg(test)]
use std::time::Duration;

impl App {
    pub(in crate::app) fn request_local<T, F, A>(&self, label: &'static str, future: F, apply: A)
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
        A: FnOnce(&mut App, &mut AppServerSession, T) -> Result<()> + Send + 'static,
    {
        let events = self.app_event_tx.clone();
        let epoch = self.server_operations.epoch();
        tokio::spawn(async move {
            let value = future.await;
            events.send(AppEvent::LocalUiReply(UiReply::from_apply(
                move |app, server| {
                    if app.server_operations.epoch() != epoch {
                        tracing::debug!(
                            operation = label,
                            "dropping local completion from retired connection"
                        );
                        return Ok(());
                    }
                    apply(app, server, value)
                },
            )));
        });
    }
}

#[cfg(test)]
impl App {
    pub(in crate::app) async fn settle_server_ui_requests(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
    ) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.server_operations.is_empty() {
                tokio::select! {
                    completion = self.server_operations.control_rx.recv() => {
                        Box::pin(self.handle_server_completion(tui, server, completion.expect("server completion"))).await?;
                    }
                    completion = self.thread_delivery.completion_rx.recv() => {
                        Box::pin(self.handle_local_thread_completion(server, completion.expect("thread completion"))).await?;
                    }
                }
            }
            Ok(())
        })
        .await?
    }
}
