//! Settlement of deferred thread-closure verification in lifecycle tests.

use super::*;

pub(super) async fn finish_close_verification(
    app: &mut App,
    tui: &mut crate::tui::Tui,
    server: &mut AppServerSession,
    events: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !app.server_operations.is_empty() {
            tokio::select! {
                completion = app.thread_delivery.completion_rx.recv() => {
                    app.handle_local_thread_completion(server, completion.expect("thread completion")).await?;
                }
                completion = app.server_operations.control_rx.recv() => {
                    app.handle_server_completion(tui, server, completion.expect("server completion")).await?;
                }
            }
        }
        Ok::<_, color_eyre::Report>(())
    }).await??;
    let mut remaining = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            AppEvent::VerifiedServerNotification(verified) => {
                app.finish_verified_server_notification(server, *verified)
                    .await
            }
            other => remaining.push(other),
        }
    }
    for event in remaining {
        app.app_event_tx.send(event);
    }
    Ok(())
}
