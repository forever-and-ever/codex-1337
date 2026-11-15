use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::Line;
use super::Stylize;
use super::ThreadId;
use super::tui;
use super::turn_submissions;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::render::renderable::Renderable;
use crate::wrapping::word_wrap_lines;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ThreadActiveFlag;
use codex_app_server_protocol::ThreadNameConflictError;
use codex_app_server_protocol::ThreadNameMode;
use codex_app_server_protocol::ThreadSetNameParams;
use codex_app_server_protocol::ThreadStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThreadNameRequest {
    Rename { thread_id: ThreadId, name: String },
    Fork { thread_id: ThreadId, name: String },
}

#[cfg(test)]
#[path = "thread_name_tests.rs"]
mod tests;

struct NameConflictHeader(Vec<Line<'static>>);

impl Renderable for NameConflictHeader {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Renderable::render(
            &Paragraph::new(word_wrap_lines(&self.0, usize::from(area.width))),
            area,
            buf,
        );
    }

    fn desired_height(&self, width: u16) -> u16 {
        u16::try_from(word_wrap_lines(&self.0, usize::from(width)).len()).unwrap_or(u16::MAX)
    }
}

impl App {
    pub(super) fn confirm_thread_name_conflict(
        &mut self,
        error: &TypedRequestError,
        request: ThreadNameRequest,
    ) -> bool {
        let TypedRequestError::Server { source, .. } = error else {
            return false;
        };
        // A partial fork already has an identity. Retrying the fork from a popup would create
        // another conversation; the server's error identifies the one to recover instead.
        if source
            .data
            .as_ref()
            .is_some_and(|data| data.get("createdThreadId").is_some())
        {
            return false;
        }
        let Some(conflict) = source
            .data
            .clone()
            .and_then(|data| serde_json::from_value::<ThreadNameConflictError>(data).ok())
        else {
            return false;
        };
        self.show_thread_name_conflict(conflict, request);
        true
    }

    fn show_thread_name_conflict(
        &mut self,
        conflict: ThreadNameConflictError,
        request: ThreadNameRequest,
    ) {
        let ThreadNameConflictError::Conflict {
            name, conflicts, ..
        } = conflict;
        let mut lines = vec![format!("“{name}” is already in use").bold().into()];
        for owner in &conflicts {
            let status = match &owner.status {
                ThreadStatus::NotLoaded => "Not loaded",
                ThreadStatus::Idle => "Idle",
                ThreadStatus::Active { active_flags }
                    if active_flags.contains(&ThreadActiveFlag::WaitingOnApproval) =>
                {
                    "Awaiting approval"
                }
                ThreadStatus::Active { active_flags }
                    if active_flags.contains(&ThreadActiveFlag::WaitingOnUserInput) =>
                {
                    "Awaiting input"
                }
                ThreadStatus::Active { .. } => "Active",
                ThreadStatus::SystemError => "Error",
            };
            lines.push(format!("{} · {status}", owner.name).into());
            lines.push(owner.thread_id.clone().into());
            lines.push(owner.cwd.as_str().to_owned().dim().into());
        }
        lines.push(
            "Archiving stops this conversation and its subagents, including running work.".into(),
        );
        let conflict_thread_ids = conflicts
            .into_iter()
            .map(|owner| owner.thread_id)
            .collect::<Vec<_>>();
        self.chat_widget.show_selection_view(SelectionViewParams {
            header: Box::new(NameConflictHeader(lines)),
            initial_selected_idx: Some(0),
            items: vec![
                SelectionItem {
                    name: "Cancel".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Archive old owners and use this name".to_string(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::ConfirmedThreadName {
                            request: request.clone(),
                            conflict_thread_ids: conflict_thread_ids.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    require_explicit_confirmation: true,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::picker()
        });
    }

    pub(super) fn rename_thread(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        name: String,
        conflict_thread_ids: Vec<String>,
    ) {
        let writer = app_server.manual_thread_name_writer(thread_id);
        self.enqueue_server_operation(
            app_server,
            thread_id,
            turn_submissions::ServerOperation::Mutation(turn_submissions::ThreadMutation::Rename(
                ThreadSetNameParams {
                    thread_id: thread_id.to_string(),
                    name,
                    naming_mode: Some(ThreadNameMode::Explicit),
                    conflict_thread_ids: (!conflict_thread_ids.is_empty())
                        .then_some(conflict_thread_ids),
                },
                writer,
            )),
        );
    }

    pub(super) fn fork_current_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        name: Option<String>,
        conflict_thread_ids: Vec<String>,
        from_agents_overview: bool,
    ) {
        let from_locked_thread = self.chat_widget.is_external_writer_view();
        let source = if from_agents_overview {
            "agents_overview_shortcut"
        } else if from_locked_thread {
            "locked_thread_shortcut"
        } else {
            "slash_command"
        };
        self.session_telemetry
            .counter("codex.thread.fork", /*inc*/ 1, &[("source", source)]);
        self.chat_widget
            .add_plain_history_lines(vec!["/fork".magenta().into()]);
        if self.chat_widget.thread_id().is_none() {
            self.chat_widget.add_error_message(
                "A thread must contain at least one turn before it can be forked.".to_string(),
            );
            return;
        }

        self.enqueue_ordinary_fork(app_server, name, conflict_thread_ids, from_agents_overview);
        tui.frame_requester().schedule_frame();
    }
}
