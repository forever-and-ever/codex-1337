//! Live UI streams transfer source ownership to the shared preparation pool.

use super::AgentMessageItem;
use super::AppEvent;
use super::Arc;
use super::ChatWidget;
use super::MessagePhase;
use super::PathBuf;
use super::PlanStreamController;
use super::StreamController;
use super::ThreadId;
use super::history_cell;
use super::parse_assistant_markdown;
use crate::markdown_render::ListSpacing;
use crate::streaming::completion::PreparedStream;

impl ChatWidget {
    pub(crate) fn admit_stream_preparation(
        &mut self,
        thread_id: ThreadId,
        item_id: String,
    ) -> PreparedStream {
        let prepared = PreparedStream::pending(thread_id, item_id);
        self.transcript.preparations.push(prepared.clone());
        prepared
    }

    pub(crate) fn take_prepared_stream(
        &mut self,
        prepared: &PreparedStream,
    ) -> Option<crate::streaming::completion::PreparedResponse> {
        let index = self
            .transcript
            .preparations
            .iter()
            .position(|pending| pending.same_origin(prepared))?;
        let response = prepared.take_ready()?;
        self.transcript.preparations.remove(index);
        Some(response)
    }

    pub(crate) fn accept_prepared_markdown(
        &mut self,
        thread_id: ThreadId,
        item_id: &str,
        markdown: crate::streaming::completion::PreparedAssistant,
    ) {
        if !markdown.markdown.is_empty()
            && (self
                .transcript
                .last_completed_agent_message
                .as_ref()
                .is_some_and(|message| message.item_id == item_id)
                || self.transcript.streaming_plan_item.as_deref() == Some(item_id))
        {
            self.transcript.last_status_copy_targets = None;
            self.transcript.last_agent_markdown = Some(markdown.markdown);
            self.transcript.last_agent_source = Some(markdown.source);
            self.transcript.saw_copy_source_this_turn = true;
        }
        if let Some(branch_cwd) = markdown.branch_cwd
            && let Some(runner) = self.workspace_command_runner.clone()
        {
            let cwd = self.config.cwd.to_path_buf();
            let tx = self.app_event_tx.clone();
            tokio::spawn(async move {
                if let Some(branch) =
                    crate::branch_summary::current_branch_name(runner.as_ref(), &branch_cwd).await
                {
                    tx.send(AppEvent::SyncThreadGitBranch {
                        thread_id,
                        branch,
                        cwd,
                    });
                }
            });
        }
    }

    pub(super) fn background_stream_enabled(&self, item_id: Option<&str>) -> bool {
        self.frame_requester.prepares_in_background()
            && self.thread_id.is_some()
            && item_id.is_some()
    }

    #[expect(
        clippy::expect_used,
        reason = "An admitted background stream retains its origin thread and item identity until consumed."
    )]
    pub(super) fn prepare_answer_stream(
        &mut self,
        controller: StreamController,
        completed: Option<&str>,
        questions: &[codex_protocol::items::AsyncUserInputQuestion],
    ) -> Option<StreamController> {
        if !controller.is_background() {
            return Some(controller);
        }
        let thread_id = self.thread_id.expect("background stream has an origin");
        let item_id = controller
            .item_id
            .clone()
            .expect("background stream has an item");
        let timestamp = controller.timestamp;
        let authoritative = completed.is_some();
        let completed = completed.map(str::to_owned);
        let questions = questions.to_vec();
        let cwd = self.config.cwd.to_path_buf();
        let context = crate::inline_visualization::InlineVisualizationContext::from_config(
            &self.config,
            thread_id,
        );
        let tx = self.app_event_tx.clone();
        self.clear_active_stream_tail();
        self.note_stream_consolidation_queued();
        let prepared = self.admit_stream_preparation(thread_id, item_id.clone());
        let result = controller.finish_background(move |source| {
            #[cfg(test)]
            crate::streaming::completion::test_support::wait(thread_id, &item_id);
            let source = completed.unwrap_or(source);
            let parsed = parse_assistant_markdown(&source, &cwd);
            let metadata = authoritative.then(|| crate::streaming::completion::PreparedAssistant {
                markdown: crate::markdown_render::followup_labels(&parsed.visible_markdown)
                    .into_owned(),
                source: source.clone(),
                branch_cwd: parsed.last_created_branch_cwd().map(PathBuf::from),
            });
            let visible = parsed.visible_markdown;
            let mut cell = history_cell::AgentMarkdownCell::new_with_inline_visualizations(
                visible, &cwd, context,
            )
            .with_questions(&questions)
            .with_copy_source(Some(source));
            cell.item_id = Some(item_id.clone());
            cell.timestamp = timestamp;
            prepared.complete(metadata, Arc::new(cell));
            tx.send(AppEvent::PreparedStream(prepared));
        });
        assert!(result.is_ok(), "background stream was checked");
        None
    }

    #[expect(
        clippy::expect_used,
        reason = "An admitted background stream retains its origin thread and item identity until consumed."
    )]
    pub(super) fn prepare_plan_stream(
        &mut self,
        controller: PlanStreamController,
        completed: Option<String>,
    ) -> Option<PlanStreamController> {
        if !controller.is_background() {
            return Some(controller);
        }
        let thread_id = self.thread_id.expect("background stream has an origin");
        let item_id = controller
            .item_id
            .clone()
            .expect("background stream has an item");
        let cwd = self.config.cwd.to_path_buf();
        let tx = self.app_event_tx.clone();
        self.clear_active_stream_tail();
        self.note_stream_consolidation_queued();
        let prepared = self.admit_stream_preparation(thread_id, item_id);
        let result = controller.finish_background(move |source| {
            let authoritative = completed.is_some();
            let source = completed.unwrap_or(source);
            let metadata = authoritative.then(|| crate::streaming::completion::PreparedAssistant {
                markdown: crate::markdown_render::followup_labels(&source).into_owned(),
                source: source.clone(),
                branch_cwd: None,
            });
            prepared.complete(
                metadata,
                Arc::new(history_cell::new_proposed_plan(source, &cwd)),
            );
            tx.send(AppEvent::PreparedStream(prepared));
        });
        assert!(result.is_ok(), "background stream was checked");
        None
    }
}

impl ChatWidget {
    pub(super) fn take_prepared_plan_stream(
        &mut self,
        completed: Option<String>,
    ) -> Option<PlanStreamController> {
        let controller = self.plan_stream_controller.take()?;
        self.prepare_plan_stream(controller, completed)
    }

    pub(super) fn take_prepared_answer_stream(
        &mut self,
        completed: Option<&str>,
        questions: &[codex_protocol::items::AsyncUserInputQuestion],
    ) -> Option<StreamController> {
        let controller = self.stream_controller.take()?;
        self.prepare_answer_stream(controller, completed, questions)
    }

    #[expect(
        clippy::expect_used,
        reason = "Background admission was checked against this controller item identity."
    )]
    pub(super) fn start_prepared_plan_stream(&mut self) {
        let mut controller = PlanStreamController::new(
            self.current_stream_width(/*reserved_cols*/ 4),
            &self.config.cwd,
            self.history_render_mode(),
        )
        .with_list_spacing(if self.local_settings.transcript_mode.is_owned() {
            ListSpacing::Compact
        } else {
            ListSpacing::AfterMultiline
        });
        controller
            .item_id
            .clone_from(&self.transcript.streaming_plan_item);
        if self.background_stream_enabled(controller.item_id.as_deref()) {
            self.add_to_history(history_cell::RespondingCell {
                item_id: controller.item_id.clone().expect("stream item"),
            });
            controller = controller.with_background(self.frame_requester.clone());
        }
        self.plan_stream_controller = Some(controller);
    }

    #[expect(
        clippy::expect_used,
        reason = "Background admission was checked against this controller item identity."
    )]
    pub(super) fn prepare_new_answer_stream(
        &mut self,
        mut controller: StreamController,
    ) -> StreamController {
        if self.background_stream_enabled(controller.item_id.as_deref()) {
            self.add_to_history(history_cell::RespondingCell {
                item_id: controller.item_id.clone().expect("stream item"),
            });
            controller = controller.with_background(self.frame_requester.clone());
        }
        controller
    }

    pub(super) fn finish_prepared_answer(
        &mut self,
        item: &AgentMessageItem,
        message: &str,
        from_replay: bool,
    ) -> bool {
        if !from_replay && self.background_stream_enabled(Some(&item.id)) {
            if self.stream_controller.is_none() {
                self.handle_streaming_delta(String::new());
            }
            if self
                .stream_controller
                .as_ref()
                .is_some_and(StreamController::is_background)
            {
                self.finalize_completed_assistant_message(
                    Some(message),
                    item.questions.as_deref().unwrap_or_default(),
                );
                self.transcript.agent_timestamp = None;
                self.status_state.pending_status_indicator_restore = item.questions.is_some()
                    || match item.phase {
                        Some(MessagePhase::FinalAnswer) => self.input_queue.pending_steer.is_some(),
                        Some(MessagePhase::Commentary | MessagePhase::PartialAnswer) | None => true,
                    };
                self.maybe_restore_status_indicator_after_stream_idle();
                return true;
            }
        }
        false
    }
}
