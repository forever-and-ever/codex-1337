//! Navigation admission waits for outstanding configuration requests.

use super::*;

impl App {
    pub(super) fn admit_overview_session(
        &mut self,
        app_server: &AppServerSession,
        cwd: Option<AbsolutePathBuf>,
    ) -> std::ops::ControlFlow<(), Option<AbsolutePathBuf>> {
        if !self.server_operations.is_empty() {
            let source = self.current_displayed_thread_id();
            self.request_server_prepared(
                app_server,
                turn_submissions::ui_requests::UiRequestScope::Connection,
                turn_submissions::ui_requests::UiRequestKind::ReadOnly,
                "Waiting to start a session",
                move |app, _| {
                    let current = app.current_displayed_thread_id() == source;
                    move |_| async move { Ok(current) }
                },
                move |app, _, ready| {
                    if matches!(ready, Ok(true)) && app.current_displayed_thread_id() == source {
                        app.app_event_tx
                            .send(AppEvent::NewAgentsOverviewSession { cwd });
                    }
                    Ok(())
                },
            );
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(cwd)
    }
}
