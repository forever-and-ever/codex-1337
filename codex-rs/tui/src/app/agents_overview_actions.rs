//! Server RPCs own lifecycle outcomes; cache cleanup follows the backend's removal scope.
//! Keep the dashboard browsable during RPCs while deferring task notifications.
//! Removing the current root leaves an unattached dashboard, even when it is empty.

use super::App;
use crate::app_event::AgentsOverviewAction;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::render::renderable::Renderable;
use crate::tui;
use crate::wrapping::word_wrap_lines;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadSectionMoveParams;
use codex_app_server_protocol::ThreadSectionMoveResponse;
use codex_protocol::ThreadId;
use codex_state::PINNED_THREAD_SECTION_ID;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use uuid::Uuid;

struct LifecycleHeader(Vec<Line<'static>>);

impl Renderable for LifecycleHeader {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Renderable::render(
            &Paragraph::new(word_wrap_lines(&self.0, usize::from(area.width))),
            area,
            buf,
        );
    }

    fn desired_height(&self, width: u16) -> u16 {
        word_wrap_lines(&self.0, usize::from(width)).len() as u16
    }
}

impl App {
    pub(super) fn toggle_agents_overview_pin(
        &mut self,
        app_server: &AppServerSession,
        thread_id: ThreadId,
        pinned: bool,
    ) {
        if self.agents_overview.pending_pin_change.is_some() {
            return;
        }
        let request_id = Uuid::new_v4();
        self.agents_overview.pending_pin_change = Some(request_id);
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = request_handle
                .request_typed::<ThreadSectionMoveResponse>(ClientRequest::ThreadSectionMove {
                    request_id: RequestId::String(Uuid::new_v4().to_string()),
                    params: ThreadSectionMoveParams {
                        thread_id: thread_id.to_string(),
                        section_id: pinned.then(|| PINNED_THREAD_SECTION_ID.to_string()),
                        before_thread_id: None,
                    },
                })
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());
            app_event_tx.send(AppEvent::AgentsOverviewPinToggled {
                request_id,
                thread_id,
                pinned,
                result,
            });
        });
    }

    pub(super) fn complete_agents_overview_pin(
        &mut self,
        app_server: &AppServerSession,
        request_id: Uuid,
        thread_id: ThreadId,
        pinned: bool,
        result: Result<(), String>,
    ) {
        if self.agents_overview.pending_pin_change != Some(request_id) {
            return;
        }
        self.agents_overview.pending_pin_change = None;
        match result {
            Ok(()) => {
                if let Some(pinned_thread_ids) = self.agents_overview.pinned_thread_ids.as_mut() {
                    if pinned {
                        if !pinned_thread_ids.contains(&thread_id) {
                            pinned_thread_ids.push(thread_id);
                        }
                    } else {
                        pinned_thread_ids.retain(|id| *id != thread_id);
                    }
                }
            }
            Err(error) => {
                self.add_agents_overview_error(format!(
                    "Failed to {} task: {error}",
                    if pinned { "pin" } else { "unpin" }
                ));
            }
        }
        let restore_show_more = self.agents_overview.refresh_show_more;
        if let Some(refresh) = self.agents_overview.refresh_task.take() {
            refresh.abort();
        }
        self.agents_overview.request_id = None;
        self.agents_overview.refresh_show_more = false;
        self.agents_overview.refresh_notifications.clear();
        let refresh_thread_ids =
            std::mem::take(&mut self.agents_overview.active_refresh_thread_ids);
        self.agents_overview
            .refresh_thread_ids
            .extend(refresh_thread_ids);
        self.agents_overview.refresh_pending = false;
        self.agents_overview.show_more_requested |= restore_show_more;
        self.refresh_agents_overview_threads(app_server);
        self.repaint_agents_overview();
    }

    pub(super) fn confirm_agents_overview_action(
        &mut self,
        thread_id: ThreadId,
        action: AgentsOverviewAction,
    ) {
        let Some(thread) = self
            .agents_overview
            .threads
            .get(&thread_id)
            .and_then(Option::as_ref)
            .or_else(|| self.agents_overview.search_matches.get(&thread_id))
        else {
            return;
        };
        let name = thread.name.as_deref().unwrap_or(&thread.preview);
        let name = name
            .trim()
            .lines()
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("Untitled task");
        let (title, description, label) = match action {
            AgentsOverviewAction::Archive => (
                format!("Archive “{name}”?"),
                "This stops any running work in this task and its child agents, then archives them. Their history can be restored from the resume picker.",
                "Archive task and child agents",
            ),
            AgentsOverviewAction::Delete => (
                format!("Permanently delete “{name}”?"),
                "This stops running work in this task and its child agents, then permanently deletes their history and saved side conversations. This cannot be undone.",
                "Permanently delete task and child agents",
            ),
        };
        self.chat_widget.show_selection_view(SelectionViewParams {
            header: Box::new(LifecycleHeader(vec![
                title.bold().into(),
                description.dim().into(),
            ])),
            items: vec![
                SelectionItem {
                    name: "Cancel".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: label.to_string(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::RunAgentsOverviewAction { thread_id, action })
                    })],
                    dismiss_on_select: true,
                    require_explicit_confirmation: action == AgentsOverviewAction::Delete,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::confirmation()
        });
    }

    pub(in crate::app) fn show_thread_removal_failure(
        &mut self,
        action: AgentsOverviewAction,
        error: &impl std::fmt::Display,
    ) {
        let verb = match action {
            AgentsOverviewAction::Archive => "archive",
            AgentsOverviewAction::Delete => "delete",
        };
        self.chat_widget.show_selection_view(SelectionViewParams {
            header: Box::new(LifecycleHeader(vec![
                format!("Could not {verb} task").red().bold().into(),
                format!("{error}").red().into(),
                "Work may have stopped. Resume the task to continue, or retry the action."
                    .dim()
                    .into(),
            ])),
            items: vec![SelectionItem {
                name: "Back to agents".into(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        });
    }

    pub(super) async fn run_agents_overview_action(
        &mut self,
        _tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        action: AgentsOverviewAction,
    ) -> color_eyre::Result<()> {
        self.enqueue_overview_removal(app_server, thread_id, action);
        Ok(())
    }
}
