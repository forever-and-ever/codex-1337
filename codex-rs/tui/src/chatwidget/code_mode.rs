//! Scoped admission observes native process bookkeeping without rendering a second lifecycle.
use super::AppEvent;
use super::ChatWidget;
use super::CollabAgentTool;
use super::CollabAgentToolCallStatus;
use super::ExecCommandSource;
use super::ReplayKind;
use super::ServerNotification;
use super::StatusDetailsCapitalization;
use super::TerminalTitleStatusKind;
use super::ThreadItem;
use super::is_unified_exec_source;
use super::multi_agents;
use crate::code_mode_transcript::Admission;
use crate::code_mode_transcript::Source;

impl ChatWidget {
    pub(super) fn admit_code_mode_snapshot(
        &mut self,
        owned: codex_app_server_protocol::CodeModeOwned<Box<ThreadItem>>,
        turn: String,
    ) {
        let admission = Admission::item(owned, turn);
        let Some(thread_id) = self.thread_id() else {
            return;
        };
        self.observe_code_mode_notification(admission.native_notification());
        self.app_event_tx.send(AppEvent::CodeModeTranscript {
            thread_id,
            admission,
        });
    }
    pub(super) fn admit_code_mode_notification(
        &mut self,
        notification: &ServerNotification,
        replay: Option<ReplayKind>,
    ) -> bool {
        let source = if replay.is_some() {
            Source::Historical
        } else {
            Source::Live
        };
        match Admission::notification(notification, source) {
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(%error, "rejecting invalid scoped transcript event");
                true
            }
            Ok(Some(admission)) => {
                let Some(thread_id) = self.thread_id() else {
                    return true;
                };
                self.empty_state_animation.borrow_mut().dismiss();
                self.observe_code_mode_notification(notification.native_notification());
                self.app_event_tx.send(AppEvent::CodeModeTranscript {
                    thread_id,
                    admission,
                });
                true
            }
        }
    }

    pub(crate) fn seal_code_mode_stream(&mut self) {
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
    }

    pub(crate) fn observe_code_mode_notification(&mut self, notification: &ServerNotification) {
        let (item, started) = match notification {
            ServerNotification::ItemStarted(event) => (&event.item, true),
            ServerNotification::ItemCompleted(event) => (&event.item, false),
            ServerNotification::CommandExecutionOutputDelta(delta) => {
                self.track_unified_exec_output_chunk(&delta.item_id, delta.delta.as_bytes());
                return;
            }
            ServerNotification::TerminalInteraction(interaction)
                if interaction.stdin.is_empty() =>
            {
                if self.bottom_pane.is_task_running() {
                    let command = self
                        .unified_exec_processes
                        .iter()
                        .find(|process| process.key == interaction.process_id)
                        .map(|process| process.command_display.clone());
                    self.bottom_pane.ensure_status_indicator();
                    self.bottom_pane
                        .set_interrupt_hint_visible(/*visible*/ true);
                    self.status_state.terminal_title_status_kind =
                        TerminalTitleStatusKind::WaitingForBackgroundTerminal;
                    self.set_status(
                        "Waiting for background terminal".into(),
                        command,
                        StatusDetailsCapitalization::Preserve,
                        /*details_max_lines*/ 1,
                    );
                }
                return;
            }
            _ => return,
        };
        match item.native_item() {
            ThreadItem::CommandExecution {
                id,
                process_id,
                command,
                source,
                ..
            } => {
                if started && *source == ExecCommandSource::UnifiedExecStartup {
                    self.track_unified_exec_process_begin(id, process_id.as_deref(), command);
                } else if !started && is_unified_exec_source(*source) {
                    if self
                        .unified_exec_wait_streak
                        .as_ref()
                        .is_some_and(|wait| Some(wait.process_id.as_str()) == process_id.as_deref())
                    {
                        self.flush_unified_exec_wait_streak();
                    }
                    self.track_unified_exec_process_end(id, process_id.as_deref());
                }
            }
            ThreadItem::CollabAgentToolCall {
                id,
                tool: CollabAgentTool::SpawnAgent,
                status,
                ..
            } => {
                if *status == CollabAgentToolCallStatus::InProgress {
                    if let Some(summary) = multi_agents::spawn_request_summary(item.native_item()) {
                        self.pending_collab_spawn_requests
                            .insert(id.clone(), summary);
                    }
                } else {
                    self.pending_collab_spawn_requests.remove(id);
                }
            }
            _ => {}
        }
    }
}
