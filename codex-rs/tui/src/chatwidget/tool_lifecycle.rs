//! Non-command tool lifecycle rendering for `ChatWidget`.
//!
//! This module handles patch, MCP, web search, image, and collaborator tool
//! events as transcript cells.

use super::*;
use crate::thread_transcript::tools::McpHistory;
use codex_utils_path_uri::LegacyAppPathString;

impl ChatWidget {
    pub(super) fn on_patch_apply_begin(
        &mut self,
        id: String,
        changes: HashMap<PathBuf, FileChange>,
    ) {
        self.add_to_history(history_cell::ToolOutputGroup::new(
            history_cell::patch_apply_cells(
                id,
                changes,
                &self.config.cwd,
                codex_app_server_protocol::PatchApplyStatus::InProgress,
            )
            .into_iter()
            .map(|cell| Arc::new(cell) as Arc<dyn HistoryCell>),
        ));
    }

    pub(super) fn on_view_image_tool_call(
        &mut self,
        call_id: String,
        path: LegacyAppPathString,
        timestamp: Option<history_cell::HistoryTimestamp>,
    ) {
        self.flush_answer_stream_with_separator();
        let origin = if self.snapshot_local_images {
            history_cell::ImagePathOrigin::Remote
        } else {
            history_cell::ImagePathOrigin::Local
        };
        let mut cell = history_cell::new_view_image_tool_call(path, origin);
        cell.timestamp = timestamp;
        self.add_to_history(history_cell::ToolItemCell::new(call_id, cell));
        self.request_redraw();
    }

    pub(super) fn on_image_generation_begin(&mut self) {
        self.flush_answer_stream_with_separator();
        if self.bottom_pane.is_task_running() {
            self.bottom_pane.ensure_status_indicator();
        }
    }

    pub(super) fn on_image_generation_end(
        &mut self,
        call_id: String,
        status: String,
        revised_prompt: Option<String>,
        saved_path: Option<AbsolutePathBuf>,
        timestamp: Option<history_cell::HistoryTimestamp>,
    ) {
        self.flush_answer_stream_with_separator();
        let mut cell = history_cell::new_image_generation_call(
            call_id.clone(),
            &status,
            revised_prompt,
            saved_path,
        );
        cell.timestamp = timestamp;
        self.add_to_history(history_cell::ToolItemCell::new(call_id, cell));
        self.request_redraw();
    }

    pub(super) fn on_file_change_completed(&mut self, item: ThreadItem) {
        self.defer_or_handle(
            item,
            InterruptManager::push_item_completed,
            Self::handle_file_change_completed_now,
        );
    }

    pub(super) fn on_mcp_tool_call_started(&mut self, item: ThreadItemPayload) {
        self.defer_or_handle(
            item,
            InterruptManager::push_item_started,
            Self::handle_mcp_tool_call_started_now,
        );
    }

    pub(super) fn on_mcp_tool_call_completed(&mut self, item: ThreadItem) {
        self.defer_or_handle(
            item,
            InterruptManager::push_item_completed,
            Self::handle_mcp_tool_call_completed_now,
        );
    }

    pub(super) fn on_web_search_begin(&mut self, call_id: String) {
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.transcript.active_cell = Some(Box::new(history_cell::new_active_web_search_call(
            call_id,
            String::new(),
            self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(super) fn on_web_search_end(
        &mut self,
        call_id: String,
        query: String,
        action: codex_app_server_protocol::WebSearchAction,
        timestamp: Option<history_cell::HistoryTimestamp>,
    ) {
        self.flush_answer_stream_with_separator();
        let mut handled = false;
        if let Some(cell) = self
            .transcript
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<WebSearchCell>())
            && cell.call_id() == call_id
        {
            cell.update(action.clone(), query.clone());
            cell.completed_at = timestamp;
            cell.complete();
            self.bump_active_cell_revision();
            self.flush_active_cell();
            handled = true;
        }

        if !handled {
            let mut cell = history_cell::new_web_search_call(call_id, query, action);
            cell.completed_at = timestamp;
            self.add_to_history(cell);
        }
    }

    pub(super) fn on_collab_event(&mut self, cell: Box<dyn HistoryCell>) {
        self.flush_answer_stream_with_separator();
        self.add_boxed_history(cell);
        self.request_redraw();
    }

    pub(super) fn on_collab_agent_tool_call(
        &mut self,
        item: ThreadItemPayload,
        timestamp: Option<history_cell::HistoryTimestamp>,
    ) {
        let ThreadItemPayload::CollabAgentToolCall {
            id, tool, status, ..
        } = &item
        else {
            return;
        };
        if matches!(tool, CollabAgentTool::SpawnAgent)
            && let Some(spawn_request) = multi_agents::spawn_request_summary(&item)
        {
            self.pending_collab_spawn_requests
                .insert(id.clone(), spawn_request);
        }

        let cached_spawn_request = if matches!(tool, CollabAgentTool::SpawnAgent)
            && !matches!(status, CollabAgentToolCallStatus::InProgress)
        {
            self.pending_collab_spawn_requests.remove(id)
        } else {
            None
        };

        if let Some(mut cell) = multi_agents::tool_call_history_cell(
            &item,
            cached_spawn_request.as_ref(),
            |thread_id| self.collab_agent_metadata(thread_id),
        ) {
            cell.timestamp = timestamp;
            self.flush_answer_stream_with_separator();
            self.add_to_history(history_cell::ToolItemCell::new(id.clone(), cell));
            self.request_redraw();
        }
    }

    pub(super) fn on_sub_agent_activity(&mut self, item: ThreadItemPayload) {
        // Background agents can finish while the parent answer is still streaming.
        // Keep that stream intact until its authoritative message completion.
        // After the turn stops, leftover prompts must not hold up late activity.
        if !self.turn_lifecycle.agent_turn_running && self.stream_controller.is_none() {
            self.handle_sub_agent_activity_now(item);
        } else {
            self.defer_or_handle(
                item,
                |interrupts, item| interrupts.push_item_completed(ThreadItem::untimed(item)),
                Self::handle_sub_agent_activity_now,
            );
        }
    }

    fn handle_sub_agent_activity_now(&mut self, item: ThreadItemPayload) {
        if let Some(cell) = multi_agents::sub_agent_activity_history_cell(&item) {
            self.on_collab_event(cell);
        }
    }

    pub(crate) fn handle_file_change_completed_now(&mut self, item: ThreadItem) {
        let ThreadItemPayload::FileChange {
            id,
            changes,
            status,
        } = item.payload
        else {
            return;
        };
        self.add_to_history(history_cell::ToolOutputGroup::new(
            history_cell::patch_apply_cells(
                id,
                file_update_changes_to_display(changes),
                &self.config.cwd,
                status,
            )
            .into_iter()
            .map(|cell| Arc::new(cell) as Arc<dyn HistoryCell>),
        ));
    }

    pub(crate) fn handle_mcp_tool_call_started_now(&mut self, item: ThreadItemPayload) {
        let ThreadItemPayload::McpToolCall {
            id,
            server,
            environment_id,
            tool,
            arguments,
            ..
        } = item
        else {
            return;
        };
        self.flush_answer_stream_with_separator();
        let invocation = McpInvocation {
            server,
            environment_id,
            tool,
            arguments: Some(arguments),
        };
        if invocation.is_computer_activity() {
            let call = history_cell::new_active_mcp_tool_call(
                id,
                invocation,
                self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
            );
            self.update_computer_activity(|cell| cell.start(call));
            self.bump_active_cell_revision();
            self.request_redraw();
            return;
        }
        self.flush_active_cell();
        self.transcript.active_cell = Some(Box::new(history_cell::new_active_mcp_tool_call(
            id,
            invocation,
            self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(crate) fn handle_mcp_tool_call_completed_now(&mut self, completed: ThreadItem) {
        self.flush_answer_stream_with_separator();

        let Some(McpHistory {
            id,
            invocation,
            duration,
            result,
        }) = McpHistory::from_item(completed.payload)
        else {
            return;
        };

        if invocation.is_computer_activity() {
            let mut call = history_cell::new_active_mcp_tool_call(
                id,
                invocation,
                self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
            );
            call.completed_at = completed
                .completed_at_ms
                .and_then(history_cell::HistoryTimestamp::from_millis);
            self.update_computer_activity(|cell| cell.complete(call, duration, result));
            self.bump_active_cell_revision();
            self.request_redraw();
            return;
        }

        match self
            .transcript
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<McpToolCallCell>())
        {
            Some(cell) if cell.call_id() == id => {
                cell.completed_at = completed
                    .completed_at_ms
                    .and_then(history_cell::HistoryTimestamp::from_millis);
                cell.complete(duration, result)
            }
            _ => {
                self.flush_active_cell();
                let mut cell = history_cell::new_active_mcp_tool_call(
                    id,
                    invocation,
                    self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
                );
                cell.completed_at = completed
                    .completed_at_ms
                    .and_then(history_cell::HistoryTimestamp::from_millis);
                cell.complete(duration, result);
                self.transcript.active_cell = Some(Box::new(cell));
            }
        };

        self.flush_active_cell();
    }

    /// Preserve pending calls and timers, returning the revisions before and after hydration.
    pub(crate) fn prepend_active_computer_history(
        &mut self,
        older: &dyn HistoryCell,
        turns: &[Turn],
    ) -> Option<(u64, u64)> {
        let previous_revision = self.transcript.active_cell_revision;
        let active = self.transcript.active_cell.as_mut().and_then(|cell| {
            cell.as_any_mut()
                .downcast_mut::<history_cell::ComputerActivityCell>()
        })?;
        let older = crate::thread_transcript::older_computer_group(older, active, turns)?;
        active.prepend(older);
        self.bump_active_cell_revision();
        Some((previous_revision, self.transcript.active_cell_revision))
    }

    /// Reuse only adjacent computer calls; all other active cells form a transcript boundary.
    fn update_computer_activity(
        &mut self,
        update: impl FnOnce(&mut history_cell::ComputerActivityCell),
    ) {
        if let Some(cell) = self.transcript.active_cell.as_mut().and_then(|cell| {
            cell.as_any_mut()
                .downcast_mut::<history_cell::ComputerActivityCell>()
        }) {
            update(cell);
        } else {
            self.flush_active_cell();
            let mut cell = history_cell::ComputerActivityCell::default();
            update(&mut cell);
            self.transcript.active_cell = Some(Box::new(cell));
        }
    }

    pub(crate) fn handle_queued_item_started_now(&mut self, item: ThreadItemPayload) {
        match item {
            item @ ThreadItemPayload::CommandExecution { .. } => {
                self.handle_command_execution_started_now(item);
            }
            item @ ThreadItemPayload::McpToolCall { .. } => {
                self.handle_mcp_tool_call_started_now(item);
            }
            item @ ThreadItemPayload::DynamicToolCall { .. } => {
                self.handle_dynamic_tool_item_now(item)
            }
            _ => {}
        }
    }

    pub(crate) fn handle_queued_item_completed_now(&mut self, item: ThreadItem) {
        match &item.payload {
            ThreadItemPayload::CommandExecution { .. } => {
                self.handle_command_execution_completed_now(item);
            }
            ThreadItemPayload::FileChange { .. } => self.handle_file_change_completed_now(item),
            ThreadItemPayload::McpToolCall { .. } => self.handle_mcp_tool_call_completed_now(item),
            ThreadItemPayload::DynamicToolCall { .. } => {
                self.handle_dynamic_tool_item_now(item.payload)
            }
            ThreadItemPayload::SubAgentActivity { .. } => {
                self.handle_sub_agent_activity_now(item.payload)
            }
            _ => {}
        }
    }
}
