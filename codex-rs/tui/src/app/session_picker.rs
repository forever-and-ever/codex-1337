//! In-app session picker transitions, preserving per-thread input and the view on cancellation.

use super::*;

impl App {
    pub(super) async fn open_resume_picker_for_backends(
        &mut self,
        _tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
    ) -> Result<AppRunControl> {
        self.enqueue_picker_preparation(app_server);
        Ok(AppRunControl::Continue)
    }

    pub(super) async fn apply_resume_picker_selection(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        selection: SessionSelection,
    ) -> Result<AppRunControl> {
        match selection {
            SessionSelection::Resume(target_session) => {
                if target_session.backend_id != app_server.backend_id() {
                    self.chat_widget.add_error_message(
                        "This conversation belongs to another backend. Open it using that backend."
                            .into(),
                    );
                    return Ok(AppRunControl::Continue);
                }
                let thread_id = target_session.thread_id;
                if let AppRunControl::Exit(reason) = self
                    .resume_target_session(tui, app_server, target_session)
                    .await?
                {
                    return Ok(AppRunControl::Exit(reason));
                }
                if self.active_thread_id == Some(thread_id)
                    && self
                        .chat_widget
                        .selected_index_for_present_view(AGENTS_OVERVIEW_VIEW_ID)
                        .is_some()
                {
                    if let Ok(mut state) = self.agents_overview.view_state.lock() {
                        state.completion = Some(crate::bottom_pane::ViewCompletion::Accepted);
                    }
                    self.chat_widget.pre_draw_tick();
                }
            }
            SessionSelection::Exit
            | SessionSelection::StartFresh
            | SessionSelection::AgentsOverview => {
                let frames = tui.frame_requester();
                self.queue_config_refresh("closing the session picker", move |app, _, _result| {
                    if app.server_operations.preview(None).navigation.is_none() {
                        app.chat_widget.maybe_send_next_queued_input();
                    }
                    frames.schedule_frame();
                    Ok(())
                });
                tui.frame_requester().schedule_frame();
                return Ok(AppRunControl::Continue);
            }
            SessionSelection::Fork(_) => {}
        }

        if self.server_operations.preview(None).navigation.is_none() {
            self.chat_widget.maybe_send_next_queued_input();
        }
        // Leaving alt-screen may blank the inline viewport; force a redraw either way.
        tui.frame_requester().schedule_frame();
        Ok(AppRunControl::Continue)
    }
}
