//! In-app session picker transitions, preserving per-thread input and the view on cancellation.

use super::*;
use crate::chatwidget::ThreadInputStateRestoreMode;

impl App {
    pub(super) async fn open_resume_picker(
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
                let thread_id = target_session.thread_id;
                let switching_threads = self.current_displayed_thread_id() != Some(thread_id);
                if switching_threads {
                    for (id, channel) in &self.thread_event_channels {
                        if let Some(input_state) = channel.store.lock().await.input_state.clone() {
                            self.agents_overview.input_states.insert(*id, input_state);
                        }
                    }
                    if let Some(current_thread_id) = self.current_displayed_thread_id()
                        && let Some(input_state) = self.chat_widget.capture_thread_input_state()
                    {
                        self.agents_overview
                            .input_states
                            .insert(current_thread_id, input_state);
                    }
                }
                if let AppRunControl::Exit(reason) = self
                    .resume_target_session(tui, app_server, target_session)
                    .await?
                {
                    return Ok(AppRunControl::Exit(reason));
                }
                if switching_threads
                    && self.current_displayed_thread_id() == Some(thread_id)
                    && let Some(input_state) = self.agents_overview.input_states.remove(&thread_id)
                {
                    let preserve_in_flight_turn =
                        self.active_turn_id_for_thread(thread_id).await.is_some();
                    self.chat_widget.restore_thread_input_state(
                        Some(input_state),
                        ThreadInputStateRestoreMode {
                            preserve_in_flight_turn,
                        },
                    );
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
                self.refresh_in_memory_config_from_disk_best_effort("closing the session picker")
                    .await;
            }
            SessionSelection::Fork(_) => {}
        }

        self.chat_widget.maybe_send_next_queued_input();
        // Leaving alt-screen may blank the inline viewport; force a redraw either way.
        tui.frame_requester().schedule_frame();
        Ok(AppRunControl::Continue)
    }
}
