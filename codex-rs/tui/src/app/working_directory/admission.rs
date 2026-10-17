//! Navigation admission waits for outstanding configuration requests.

use super::*;

impl App {
    pub(super) fn admit_directory_change(
        &mut self,
        app_server: &AppServerSession,
        cwd: AbsolutePathBuf,
    ) -> std::ops::ControlFlow<(), AbsolutePathBuf> {
        if !self.server_operations.is_empty() {
            let Some(thread_id) = self.chat_widget.thread_id() else {
                return std::ops::ControlFlow::Break(());
            };
            self.supersede_pending_navigation();
            let generation = self.navigation_generation();
            self.request_server_prepared(
                app_server,
                super::super::turn_submissions::ui_requests::UiRequestScope::Connection,
                super::super::turn_submissions::ui_requests::UiRequestKind::ReadOnly,
                "Waiting for directory-change settings",
                move |app, _| {
                    let current = app.current_displayed_thread_id() == Some(thread_id)
                        && app.navigation_generation() == generation;
                    move |_| async move { Ok(current) }
                },
                move |app, _, ready| {
                    if matches!(ready, Ok(true))
                        && app.current_displayed_thread_id() == Some(thread_id)
                        && app.navigation_generation() == generation
                    {
                        app.app_event_tx.send(AppEvent::DirectoryChangeAdmitted {
                            thread_id,
                            cwd,
                            generation,
                        });
                    }
                    Ok(())
                },
            );
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(cwd)
    }
}
