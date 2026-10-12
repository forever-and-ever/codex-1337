use super::*;
use crate::app_server_session::ForkGoalContinuation;
use crate::app_server_session::ForkName;
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
        word_wrap_lines(&self.0, usize::from(width)).len() as u16
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

    pub(super) async fn rename_thread(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        name: String,
        conflict_thread_ids: Vec<String>,
    ) {
        self.enqueue_server_operation(
            app_server,
            thread_id,
            super::turn_submissions::ThreadOperation::Mutation(
                super::turn_submissions::ThreadMutation::Rename(ThreadSetNameParams {
                    thread_id: thread_id.to_string(),
                    name,
                    naming_mode: Some(ThreadNameMode::Explicit),
                    conflict_thread_ids: (!conflict_thread_ids.is_empty())
                        .then_some(conflict_thread_ids),
                }),
            ),
        );
    }

    pub(super) async fn fork_current_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        name: Option<String>,
        conflict_thread_ids: Vec<String>,
        from_agents_overview: bool,
    ) -> Result<()> {
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
        let Some(thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.add_error_message(
                "A thread must contain at least one turn before it can be forked.".to_string(),
            );
            return Ok(());
        };
        if self.pending_server_profiles.contains_key(&thread_id) {
            self.chat_widget
                .add_error_message("Wait for permissions to update before forking.".into());
            return Ok(());
        }
        self.chat_widget.fork_in_progress = true;
        let screen_size = tui.terminal.last_known_screen_size;
        self.handle_draw_pre_render(tui, screen_size)?;
        self.chat_widget.pre_draw_tick();
        self.render_chat_widget_frame(tui, screen_size)?;
        self.refresh_in_memory_config_from_disk_best_effort("forking the thread")
            .await;
        let mut config = if self.chat_widget.is_parent_owned_thread() {
            let mut config = self.chat_widget.config_ref().clone();
            config.service_tier = self.chat_widget.current_service_tier().map(str::to_string);
            config
        } else {
            self.config.clone()
        };
        if app_server.uses_remote_workspace() {
            config
                .workspace_roots
                .clone_from(&self.chat_widget.config_ref().workspace_roots);
        }
        config
            .model_provider_id
            .clone_from(&self.chat_widget.config_ref().model_provider_id);
        config.model = Some(self.chat_widget.current_model().to_string());
        config.model_reasoning_effort = self.chat_widget.current_reasoning_effort();
        config.daybreak_enabled = self.chat_widget.daybreak_enabled;
        let selected_profile = self.selected_server_profile(thread_id);
        // Sampled once: the source turn may finish while the fork request is in flight.
        let source_turn_running = self.chat_widget.is_agent_turn_running();
        let goal_continuation = if source_turn_running {
            ForkGoalContinuation::DeferUntilNextTurn
        } else {
            ForkGoalContinuation::StartIfIdle
        };
        let result = app_server
            .fork_thread_at(
                &self.local_settings,
                config,
                thread_id,
                /*last_turn_id*/ None,
                /*before_turn_id*/ None,
                goal_continuation,
                selected_profile.as_ref(),
                name.clone().map(|name| ForkName {
                    name,
                    conflict_thread_ids,
                }),
            )
            .await;
        self.chat_widget.fork_in_progress = false;
        match result {
            Ok(forked) => {
                if let Some(profile) = &selected_profile {
                    self.agents_overview
                        .selected_permission_profiles
                        .insert(forked.session.thread_id, profile.profile_id.clone());
                }
                // In-flight input stays with its source; only idle locked drafts follow the fork.
                let retained_input = (from_locked_thread && !source_turn_running)
                    .then(|| self.chat_widget.capture_thread_input_state())
                    .flatten();
                // Fork changes the foreground, not the source family's lifetime or approvals.
                let mut retained_threads = self.agent_navigation.tracked_thread_ids();
                retained_threads.push(thread_id);
                retained_threads.extend(
                    self.thread_event_channels
                        .keys()
                        .copied()
                        .filter(|id| self.is_family_thread(*id)),
                );
                for id in retained_threads {
                    if !self.side_threads.contains_key(&id) {
                        self.agents_overview
                            .dispatched_requests
                            .entry(id)
                            .or_default();
                        if let Some(channel) = self.thread_event_channels.get(&id)
                            && let Some(input) = channel.store.lock().await.input_state.clone()
                        {
                            self.agents_overview.input_states.insert(id, input);
                        }
                    }
                }
                if let Some(input) = self.chat_widget.capture_thread_input_state() {
                    self.agents_overview.input_states.insert(thread_id, input);
                }
                self.detach_current_thread_for_navigation(
                    app_server,
                    Some(forked.session.thread_id),
                )
                .await;
                match self
                    .replace_chat_widget_with_app_server_thread(
                        tui,
                        forked,
                        super::session_lifecycle::ThreadAttachPresentation::SessionLineage,
                        /*initial_user_message*/ None,
                    )
                    .await
                {
                    Ok(()) => {
                        self.chat_widget
                            .restore_reconnected_input(retained_input, &[]);
                        self.chat_widget.add_info_message(
                            "Fork created. You can continue here.".to_string(),
                            /*hint*/ None,
                        );
                    }
                    Err(error) => self.chat_widget.add_error_message(format!(
                        "Failed to attach to forked app-server thread: {error}"
                    )),
                }
            }
            Err(error) => {
                let confirmed = name.is_some_and(|name| {
                    error
                        .downcast_ref::<TypedRequestError>()
                        .is_some_and(|error| {
                            self.confirm_thread_name_conflict(
                                error,
                                ThreadNameRequest::Fork { thread_id, name },
                            )
                        })
                });
                if !confirmed {
                    self.chat_widget.add_error_message(format!(
                        "Failed to fork current session through the app server: {error}"
                    ));
                }
            }
        }
        if (from_locked_thread || from_agents_overview)
            && tui.presentation_mode() != crate::tui::PresentationMode::Frozen
        {
            if let Err(error) = tui.discard_pending_input_before_interactive_screen() {
                tracing::warn!(%error, "failed to discard input after forking");
            }
            tui.schedule_screen_size_recheck(Duration::ZERO);
        }
        Ok(())
    }
}
