//! Goal box requests (codex-1337): `/goal timebox` and `/goal quotabox`.

use super::super::turn_submissions::ui_requests::UiRequestKind;
use super::super::turn_submissions::ui_requests::UiRequestScope;
use super::App;
use super::AppServerSession;
use super::ThreadId;
use super::thread_goal_error_message;
use crate::app_server_session::typed_requests::SessionRpc;
use crate::goal_boxes::GoalBoxCommand;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadGoalBoxesReadParams;
use codex_app_server_protocol::ThreadGoalQuotaboxSetParams;
use codex_app_server_protocol::ThreadGoalTimeboxSetParams;
use uuid::Uuid;

impl App {
    pub(in crate::app) fn thread_goal_boxes(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        command: GoalBoxCommand,
    ) {
        let (kind, action) = match command {
            GoalBoxCommand::Show => (UiRequestKind::ReadOnly, "read"),
            GoalBoxCommand::Timebox(_) | GoalBoxCommand::Quotabox(_) => {
                (UiRequestKind::Mutation, "box")
            }
        };
        self.request_server(
            app_server,
            UiRequestScope::DisplayedThread(thread_id),
            kind,
            "Goal boxes",
            move |handle| async move {
                let request_id = RequestId::String(format!("tui-goal-boxes-{}", Uuid::new_v4()));
                let thread_id = thread_id.to_string();
                Ok(match command {
                    GoalBoxCommand::Show => {
                        let response = handle
                            .thread_goal_boxes_read(
                                request_id,
                                ThreadGoalBoxesReadParams { thread_id },
                            )
                            .await?;
                        (response.boxes, response.gauge)
                    }
                    GoalBoxCommand::Timebox(deadline) => {
                        let response = handle
                            .thread_goal_timebox_set(
                                request_id,
                                ThreadGoalTimeboxSetParams {
                                    thread_id,
                                    deadline,
                                },
                            )
                            .await?;
                        (response.boxes, None)
                    }
                    GoalBoxCommand::Quotabox(quotabox) => {
                        let response = handle
                            .thread_goal_quotabox_set(
                                request_id,
                                ThreadGoalQuotaboxSetParams {
                                    thread_id,
                                    quotabox,
                                },
                            )
                            .await?;
                        (response.boxes, response.gauge)
                    }
                })
            },
            move |app, _, result| {
                match result {
                    Ok((boxes, gauge)) => {
                        let note = command.note(boxes, gauge.as_ref());
                        app.chat_widget
                            .add_info_message(note.headline.to_string(), note.detail);
                    }
                    Err(error) => app.chat_widget.add_error_message(thread_goal_error_message(
                        action,
                        &color_eyre::eyre::eyre!(error.to_string()),
                    )),
                }
                Ok(())
            },
        );
    }
}
