//! Settlement of local and server requests in app tests.

use super::*;

pub(in crate::app) async fn settle_frontend_requests(
    app: &mut App,
    tui: &mut tui::Tui,
    server: &mut AppServerSession,
    events: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    Box::pin(tokio::time::timeout(Duration::from_secs(10), async {
        while app.active_export.is_some()
            || app.pending_revert.is_some()
            || app.pending_thread_switch_resets != 0
            || !events.is_empty()
        {
            tokio::select! {
                Some(event) = events.recv() => {
                    Box::pin(app.handle_event(tui, server, event)).await?;
                }
                Some(completion) = app.server_operations.control_rx.recv() => {
                    Box::pin(app.handle_server_completion(tui, server, completion)).await?;
                }
                Some(completion) = app.thread_delivery.completion_rx.recv() => {
                    app.handle_local_thread_completion(server, completion).await?;
                }
                Some(event) = server.next_event() => app.handle_backend_event(server, event).await,
                Some(event) = async {
                    match app.active_thread_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => app.handle_active_thread_event(tui, server, event).await?,
            }
        }
        Ok::<_, color_eyre::Report>(())
    }))
    .await??;
    Ok(())
}

impl App {
    pub(in crate::app) async fn refresh_test_config(&mut self) -> Result<()> {
        let projected = self
            .capture_config_rebuild(self.chat_widget.config_ref().cwd.to_path_buf(), None)
            .run()
            .await?;
        self.install_refreshed_config(projected.0)
    }
}

/// Wait for actual worker-requested Draw events, retaining the last frame on failure.
pub(in crate::app) async fn settle_until_painted(
    app: &mut App,
    tui: &mut tui::Tui,
    mut paint: impl FnMut(&mut App, &mut tui::Tui) -> Result<String>,
    predicate: impl Fn(&str) -> bool,
) -> Result<String> {
    // Subscribe before preparation can request its first frame.
    let mut events = tui::test_support::draw_requests(tui);
    let mut last = paint(app, tui)?;
    if predicate(&last) {
        return Ok(last);
    }
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(()) => {
                    last = paint(app, tui)?;
                    if predicate(&last) {
                        return Ok(());
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    color_eyre::eyre::bail!("Draw stream ended before the requested frame");
                }
            }
        }
    })
    .await;
    match result {
        Ok(Ok(())) => Ok(last),
        Ok(Err(error)) => Err(error.wrap_err(format!("Last frame:\n{last}"))),
        Err(error) => Err(color_eyre::eyre::eyre!(
            "Frame was not ready: {error}\nLast frame:\n{last}"
        )),
    }
}
