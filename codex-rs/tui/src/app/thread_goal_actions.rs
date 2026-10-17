use super::App;
use crate::app_event::AppEvent;
use crate::app_event::ThreadGoalSetMode;
use crate::app_server_session::AppServerSession;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::goal_display::GOAL_USAGE;
use crate::goal_display::goal_status_label;
use crate::goal_display::goal_usage_summary;
use crate::goal_files;
use crate::text_formatting::truncate_text;
use codex_app_server_protocol::ThreadGoal;
use codex_app_server_protocol::ThreadGoalStatus;
use codex_protocol::ThreadId;

use super::model_defaults::request_failure_message;
use super::turn_submissions::ui_requests::{UiRequestKind, UiRequestScope};
use codex_app_server_client::{AppServerRequestHandle, TypedRequestError};
use codex_app_server_protocol::{
    ClientRequest, RequestId, ThreadGoalClearParams, ThreadGoalClearResponse, ThreadGoalGetParams,
    ThreadGoalGetResponse, ThreadGoalMutationOrigin, ThreadGoalSetParams, ThreadGoalSetResponse,
};
use uuid::Uuid;

const EPHEMERAL_THREAD_GOAL_ERROR_MESSAGE: &str = concat!(
    "Goals need a saved session. This session is temporary.\n",
    "Run `codex` to start a saved session, or `codex resume` / `/resume` to reopen one.",
);

impl App {
    pub(super) async fn open_thread_goal_menu(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        self.request_server(
            app_server,
            UiRequestScope::DisplayedThread(thread_id),
            UiRequestKind::ReadOnly,
            "Read goal",
            move |handle| get_goal(handle, thread_id),
            move |app, _, result| {
                match result {
                    Ok(response) => match response.goal {
                        Some(goal) => app.chat_widget.show_goal_summary(goal),
                        None => app.chat_widget.add_info_message(
                            GOAL_USAGE.to_string(),
                            Some("No goal is currently set.".to_string()),
                        ),
                    },
                    Err(error) => app
                        .chat_widget
                        .add_error_message(goal_request_error("read", error)),
                }
                Ok(())
            },
        );
    }

    pub(super) async fn maybe_prompt_resume_paused_goal_after_resume(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        if self.chat_widget.has_misalignment_policy_violation() {
            return;
        }
        self.request_server(
            app_server,
            UiRequestScope::DisplayedThread(thread_id),
            UiRequestKind::ReadOnly,
            "Read resumed goal",
            move |handle| get_goal(handle, thread_id),
            move |app, _, result| {
                match result {
                    Ok(response) => {
                        if let Some(goal) = response.goal
                            && matches!(
                                goal.status,
                                ThreadGoalStatus::Paused
                                    | ThreadGoalStatus::Blocked
                                    | ThreadGoalStatus::UsageLimited
                            )
                            && !app.chat_widget.has_misalignment_policy_violation()
                        {
                            app.chat_widget
                                .show_resume_paused_goal_prompt(thread_id, goal.objective);
                        }
                    }
                    Err(error) => tracing::warn!(
                        "failed to read thread goal after resume: {}",
                        request_failure_message(error)
                    ),
                }
                Ok(())
            },
        );
    }

    pub(super) async fn open_thread_goal_editor(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: Option<ThreadId>,
    ) {
        let Some(thread_id) = thread_id else {
            self.show_no_thread_goal_to_edit();
            return;
        };
        let codex_home = app_server.codex_home_path(&self.config.codex_home);
        let fs = app_server.file_system();
        self.request_server(
            app_server,
            UiRequestScope::DisplayedThread(thread_id),
            UiRequestKind::ReadOnly,
            "Read goal for editing",
            move |handle| async move {
                let response = get_goal(handle, thread_id).await?;
                let Some(mut goal) = response.goal else {
                    return Ok(None);
                };
                let error = match goal_files::objective_text_for_edit(
                    &fs,
                    codex_home.as_ref(),
                    &goal.objective,
                )
                .await
                {
                    Ok(objective) => {
                        goal.objective = objective;
                        None
                    }
                    Err(error) => Some(error.to_string()),
                };
                Ok(Some((goal, error)))
            },
            move |app, _, result| {
                match result {
                    Ok(Some((goal, error))) => {
                        if let Some(error) = error {
                            app.chat_widget.add_error_message(error);
                        }
                        app.chat_widget.show_goal_edit_prompt(thread_id, goal);
                    }
                    Ok(None) => app.show_no_thread_goal_to_edit(),
                    Err(error) => app
                        .chat_widget
                        .add_error_message(goal_request_error("read", error)),
                }
                Ok(())
            },
        );
    }

    pub(super) async fn set_thread_goal_draft(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        draft: goal_files::GoalDraft,
        mode: ThreadGoalSetMode,
    ) {
        if matches!(mode, ThreadGoalSetMode::ConfirmIfExists) {
            self.request_server(
                app_server,
                UiRequestScope::DisplayedThread(thread_id),
                UiRequestKind::ReadOnly,
                "Read goal before replacing",
                move |handle| get_goal(handle, thread_id),
                move |app, server, result| {
                    match result {
                        Ok(response) => match response.goal.as_ref() {
                            Some(goal) if should_confirm_before_replacing_goal(goal) => {
                                app.show_replace_thread_goal_confirmation(thread_id, draft)
                            }
                            existing => app.enqueue_goal_draft(
                                server,
                                thread_id,
                                draft,
                                if existing.is_some() {
                                    ThreadGoalSetMode::ReplaceExisting
                                } else {
                                    mode
                                },
                            ),
                        },
                        Err(error) => app
                            .chat_widget
                            .add_error_message(goal_request_error("read", error)),
                    }
                    Ok(())
                },
            );
        } else {
            self.enqueue_goal_draft(app_server, thread_id, draft, mode);
        }
    }

    fn enqueue_goal_draft(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        draft: goal_files::GoalDraft,
        mode: ThreadGoalSetMode,
    ) {
        let codex_home = server.codex_home_path(&self.config.codex_home);
        let fs = server.file_system();
        let replacing_goal = matches!(mode, ThreadGoalSetMode::ReplaceExisting);
        self.request_server(
            server,
            UiRequestScope::DisplayedThread(thread_id),
            UiRequestKind::Mutation,
            "Save goal",
            move |handle| async move {
                let (objective, output_dir) =
                    match goal_files::materialize_goal_draft(&fs, codex_home.as_ref(), draft).await
                    {
                        Ok(value) => value,
                        Err(error) => return Ok(Err(error.to_string())),
                    };
                let result = async {
                    if replacing_goal {
                        clear_goal(handle.clone(), thread_id).await?;
                    }
                    let (status, token_budget) = match mode {
                        ThreadGoalSetMode::ConfirmIfExists | ThreadGoalSetMode::ReplaceExisting => {
                            (ThreadGoalStatus::Active, None)
                        }
                        ThreadGoalSetMode::UpdateExisting {
                            status,
                            token_budget,
                        } => (status, Some(token_budget)),
                    };
                    set_goal(handle, thread_id, Some(objective), status, token_budget).await
                }
                .await;
                // Only a definitive rejection permits deleting files: an uncertain set may refer to them.
                if matches!(&result, Err(TypedRequestError::Server { .. })) {
                    cleanup_materialized_goal_files(&fs, output_dir).await;
                }
                result.map(Ok)
            },
            move |app, _, result| {
                match result {
                    Ok(Ok(response)) => {
                        app.chat_widget.add_info_message(
                            format!("Goal {}", goal_status_label(response.goal.status)),
                            Some(goal_usage_summary(&response.goal)),
                        );
                        app.chat_widget.maybe_send_next_queued_input();
                    }
                    Ok(Err(error)) => app.chat_widget.add_error_message(error),
                    Err(error) => app.chat_widget.add_error_message(goal_request_error(
                        if replacing_goal { "replace" } else { "set" },
                        error,
                    )),
                }
                Ok(())
            },
        );
    }

    pub(super) async fn set_thread_goal_status(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        status: ThreadGoalStatus,
    ) {
        self.request_server(
            app_server,
            UiRequestScope::DisplayedThread(thread_id),
            UiRequestKind::Mutation,
            "Update goal",
            move |handle| set_goal(handle, thread_id, None, status, None),
            move |app, _, result| {
                match result {
                    Ok(response) => app.chat_widget.add_info_message(
                        format!("Goal {}", goal_status_label(response.goal.status)),
                        Some(goal_usage_summary(&response.goal)),
                    ),
                    Err(error) => app
                        .chat_widget
                        .add_error_message(goal_request_error("update", error)),
                }
                Ok(())
            },
        );
    }

    pub(super) async fn clear_thread_goal(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        self.request_server(
            app_server,
            UiRequestScope::DisplayedThread(thread_id),
            UiRequestKind::Mutation,
            "Clear goal",
            move |handle| clear_goal(handle, thread_id),
            move |app, _, result| {
                match result {
                    Ok(response) if response.cleared => app
                        .chat_widget
                        .add_info_message("Goal cleared".to_string(), None),
                    Ok(_) => app.chat_widget.add_info_message(
                        "No goal to clear".to_string(),
                        Some("This thread does not currently have a goal.".to_string()),
                    ),
                    Err(error) => app
                        .chat_widget
                        .add_error_message(goal_request_error("clear", error)),
                }
                Ok(())
            },
        );
    }

    pub(super) fn show_replace_thread_goal_confirmation(
        &mut self,
        thread_id: ThreadId,
        draft: goal_files::GoalDraft,
    ) {
        let objective = draft.objective.clone();
        let replace_draft = draft;
        let replace_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::SetThreadGoalDraft {
                thread_id,
                draft: replace_draft.clone(),
                mode: ThreadGoalSetMode::ReplaceExisting,
            });
        })];
        let items = vec![
            SelectionItem {
                name: "Replace current goal".to_string(),
                description: Some("Set the new objective and start it now".to_string()),
                actions: replace_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Cancel".to_string(),
                description: Some("Keep the current goal".to_string()),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Replace goal?".to_string()),
            subtitle: Some(format!(
                "New objective: {}",
                truncate_text(&objective, /*max_graphemes*/ 200)
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..SelectionViewParams::confirmation()
        });
    }

    fn show_no_thread_goal_to_edit(&mut self) {
        self.chat_widget
            .add_error_message("No goal is currently set.".to_string());
        self.chat_widget.add_info_message(
            GOAL_USAGE.to_string(),
            Some("Create a goal before editing it.".to_string()),
        );
    }
}

async fn cleanup_materialized_goal_files(
    app_server: &crate::app_server_session::fs::AppServerFileSystem,
    output_dir: Option<goal_files::GoalFilePath>,
) {
    if let Some(output_dir) = output_dir
        && let Err(err) = app_server.fs_remove_path(&output_dir).await
    {
        tracing::warn!("failed to clean up materialized goal files at {output_dir}: {err}");
    }
}

async fn get_goal(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
) -> Result<ThreadGoalGetResponse, TypedRequestError> {
    handle
        .request_typed(ClientRequest::ThreadGoalGet {
            request_id: RequestId::String(format!("tui-goal-read-{}", Uuid::new_v4())),
            params: ThreadGoalGetParams {
                thread_id: thread_id.to_string(),
            },
        })
        .await
}

async fn clear_goal(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
) -> Result<ThreadGoalClearResponse, TypedRequestError> {
    handle
        .request_typed(ClientRequest::ThreadGoalClear {
            request_id: RequestId::String(format!("tui-goal-clear-{}", Uuid::new_v4())),
            params: ThreadGoalClearParams {
                thread_id: thread_id.to_string(),
                origin: Some(ThreadGoalMutationOrigin::User),
            },
        })
        .await
}

async fn set_goal(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    objective: Option<String>,
    status: ThreadGoalStatus,
    token_budget: Option<Option<i64>>,
) -> Result<ThreadGoalSetResponse, TypedRequestError> {
    handle
        .request_typed(ClientRequest::ThreadGoalSet {
            request_id: RequestId::String(format!("tui-goal-set-{}", Uuid::new_v4())),
            params: ThreadGoalSetParams {
                thread_id: thread_id.to_string(),
                origin: Some(ThreadGoalMutationOrigin::User),
                objective,
                status: Some(status),
                token_budget,
            },
        })
        .await
}

fn goal_request_error(action: &str, error: &super::turn_submissions::SubmissionFailure) -> String {
    thread_goal_error_message(
        action,
        &color_eyre::eyre::eyre!(request_failure_message(error)),
    )
}

fn thread_goal_error_message(action: &str, err: &color_eyre::Report) -> String {
    if is_ephemeral_thread_goal_error(err) {
        EPHEMERAL_THREAD_GOAL_ERROR_MESSAGE.to_string()
    } else {
        format!("Failed to {action} thread goal: {err}")
    }
}

fn is_ephemeral_thread_goal_error(err: &color_eyre::Report) -> bool {
    err.chain().any(|cause| {
        let message = cause.to_string();
        message.contains("ephemeral thread does not support goals")
            || message.contains("thread goals require a persisted thread; this thread is ephemeral")
    })
}

fn should_confirm_before_replacing_goal(goal: &ThreadGoal) -> bool {
    // Completed goals are terminal, so `/goal <objective>` can start a fresh goal
    // without asking the user to confirm replacing already-finished work.
    match goal.status {
        ThreadGoalStatus::Complete => false,
        ThreadGoalStatus::Active
        | ThreadGoalStatus::Paused
        | ThreadGoalStatus::Blocked
        | ThreadGoalStatus::UsageLimited
        | ThreadGoalStatus::BudgetLimited => true,
    }
}

#[cfg(test)]
mod tests {
    use crate::history_cell::HistoryCell;
    use pretty_assertions::assert_eq;
    use ratatui::layout::Rect;

    use super::*;

    #[test]
    fn thread_goal_error_message_explains_temporary_session() {
        let err = color_eyre::eyre::eyre!(
            "thread/goal/get failed: ephemeral thread does not support goals: thread-1"
        )
        .wrap_err("thread/goal/get failed in TUI");

        assert_eq!(
            thread_goal_error_message("read", &err),
            EPHEMERAL_THREAD_GOAL_ERROR_MESSAGE
        );
    }

    #[test]
    fn thread_goal_ephemeral_error_message_renders_snapshot() {
        let err = color_eyre::eyre::eyre!(
            "thread/goal/get failed: ephemeral thread does not support goals: thread-1"
        )
        .wrap_err("thread/goal/get failed in TUI");
        let cell = crate::history_cell::new_error_event(thread_goal_error_message("read", &err));
        let width = 72;
        let height = 6;
        let backend = crate::test_backend::VT100Backend::new(width, height);
        let mut terminal =
            crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, height - 1, width, 1));

        crate::insert_history::insert_history_lines(
            &mut terminal,
            cell.display_lines(/*width*/ width),
        )
        .expect("insert history lines");

        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn thread_goal_error_message_preserves_generic_failure_context() {
        let err =
            color_eyre::eyre::eyre!("server disappeared").wrap_err("thread/goal/get failed in TUI");

        assert_eq!(
            thread_goal_error_message("read", &err),
            "Failed to read thread goal: thread/goal/get failed in TUI"
        );
    }

    #[test]
    fn completed_goal_does_not_require_replace_confirmation() {
        assert!(!should_confirm_before_replacing_goal(&test_goal(
            ThreadGoalStatus::Complete
        )));
    }

    #[test]
    fn unfinished_goals_require_replace_confirmation() {
        for status in [
            ThreadGoalStatus::Active,
            ThreadGoalStatus::Paused,
            ThreadGoalStatus::Blocked,
            ThreadGoalStatus::UsageLimited,
            ThreadGoalStatus::BudgetLimited,
        ] {
            assert!(should_confirm_before_replacing_goal(&test_goal(status)));
        }
    }

    fn test_goal(status: ThreadGoalStatus) -> ThreadGoal {
        ThreadGoal {
            thread_id: ThreadId::new().to_string(),
            objective: "Finish the thing.".to_string(),
            status,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: 1_776_272_400,
            updated_at: 1_776_272_460,
        }
    }
}
