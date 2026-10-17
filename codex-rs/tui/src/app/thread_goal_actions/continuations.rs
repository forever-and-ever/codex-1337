//! Deferred goal reads, file preparation, and goal mutations.

use super::super::turn_submissions::ui_requests::UiRequestKind;
use super::super::turn_submissions::ui_requests::UiRequestScope;
use super::*;
use crate::app_server_session::typed_requests::AppServerRequestHandleExt;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadGoalClearParams;
use codex_app_server_protocol::ThreadGoalClearResponse;
use codex_app_server_protocol::ThreadGoalGetParams;
use codex_app_server_protocol::ThreadGoalGetResponse;
use codex_app_server_protocol::ThreadGoalMutationOrigin;
use codex_app_server_protocol::ThreadGoalSetParams;
use codex_app_server_protocol::ThreadGoalSetResponse;
use uuid::Uuid;

async fn get_goal(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
) -> Result<ThreadGoalGetResponse, TypedRequestError> {
    handle
        .thread_goal_get(
            RequestId::String(format!("tui-goal-read-{}", Uuid::new_v4())),
            ThreadGoalGetParams {
                thread_id: thread_id.to_string(),
            },
        )
        .await
}

async fn clear_goal(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
) -> Result<ThreadGoalClearResponse, TypedRequestError> {
    handle
        .thread_goal_clear(
            RequestId::String(format!("tui-goal-clear-{}", Uuid::new_v4())),
            ThreadGoalClearParams {
                thread_id: thread_id.to_string(),
                origin: Some(ThreadGoalMutationOrigin::User),
            },
        )
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
        .thread_goal_set(
            RequestId::String(format!("tui-goal-set-{}", Uuid::new_v4())),
            ThreadGoalSetParams {
                thread_id: thread_id.to_string(),
                origin: Some(ThreadGoalMutationOrigin::User),
                objective,
                status: Some(status),
                token_budget,
            },
        )
        .await
}

fn goal_request_error(
    action: &str,
    error: &super::super::turn_submissions::SubmissionFailure,
) -> String {
    thread_goal_error_message(action, &color_eyre::eyre::eyre!(error.to_string()))
}

impl App {
    pub(super) async fn open_thread_goal_menu_continuation(
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

    pub(super) async fn maybe_prompt_resume_paused_goal_after_resume_continuation(
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
                    Err(error) => {
                        tracing::warn!("failed to read thread goal after resume: {}", error)
                    }
                }
                Ok(())
            },
        );
    }

    pub(super) async fn open_thread_goal_editor_continuation(
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

    pub(super) async fn set_thread_goal_draft_continuation(
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

    pub(super) fn enqueue_goal_draft(
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

    pub(super) async fn set_thread_goal_status_continuation(
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

    pub(super) async fn clear_thread_goal_continuation(
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
}
