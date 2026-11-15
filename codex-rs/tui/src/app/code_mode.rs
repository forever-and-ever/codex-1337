//! Apply canonical scoped blocks through the existing retained-history/reading snapshot boundary.
use super::App;
use super::AppEvent;
use super::Arc;
use super::HistoryCell;
use super::Overlay;
use super::ServerNotification;
use super::ThreadId;
use super::history_cell;
use super::transcript_splice::TranscriptSplice;
use super::tui;
use crate::code_mode_transcript::Admission;
use crate::code_mode_transcript::Projection;
use crate::code_mode_transcript::Source;
use crate::code_mode_transcript::TokenBill;

impl App {
    pub(super) fn prepare_live_code_mode(&mut self, notification: &ServerNotification) -> bool {
        let admission = match Admission::notification(notification, Source::Live) {
            Ok(None) => return false,
            Ok(Some(admission)) => admission,
            Err(error) => {
                tracing::warn!(%error, "rejecting invalid owned notification");
                return true;
            }
        };
        let Some(display) = self.chat_widget.thread_id() else {
            return true;
        };
        if display != admission.owner.thread_id {
            tracing::warn!("rejecting misrouted live owned notification");
            return true;
        }
        let animations =
            self.local_settings.tui.animations && self.local_settings.tui.effects.progress;
        let projection = match self.code_mode_transcript.admit(
            display,
            admission,
            &self.chat_widget.config_ref().cwd,
            animations,
            &|thread| self.chat_widget.collab_agent_metadata(thread),
            if self.app_server_target.uses_remote_workspace() {
                history_cell::ImagePathOrigin::Remote
            } else {
                history_cell::ImagePathOrigin::Local
            },
        ) {
            Ok(projection) => projection,
            Err(error) => {
                tracing::warn!(%error, "rejecting conflicting live owned notification");
                return true;
            }
        };
        if projection.opens_live {
            self.chat_widget.seal_code_mode_stream();
        }
        crate::shimmers::observe(notification);
        self.chat_widget
            .observe_code_mode_notification(notification.native_notification());
        // The seal's ordinary history events precede the newly opened block.
        self.app_event_tx.send(AppEvent::CodeModeProjection {
            thread_id: display,
            projection,
        });
        true
    }

    pub(super) fn apply_code_mode_admission(
        &mut self,
        tui: &mut tui::Tui,
        display: ThreadId,
        admission: Admission,
    ) {
        if self.chat_widget.thread_id() != Some(display) {
            return;
        }
        let animations =
            self.local_settings.tui.animations && self.local_settings.tui.effects.progress;
        let projection = match self.code_mode_transcript.admit(
            display,
            admission,
            &self.chat_widget.config_ref().cwd,
            animations,
            &|thread| self.chat_widget.collab_agent_metadata(thread),
            if self.app_server_target.uses_remote_workspace() {
                history_cell::ImagePathOrigin::Remote
            } else {
                history_cell::ImagePathOrigin::Local
            },
        ) {
            Ok(projection) => projection,
            Err(error) => {
                tracing::warn!(%error, "rejecting conflicting Code Mode update");
                return;
            }
        };
        self.apply_code_mode_projection(tui, projection);
    }

    pub(super) fn apply_code_mode_projection(
        &mut self,
        tui: &mut tui::Tui,
        projection: Projection,
    ) {
        if let Some(TokenBill {
            item_id,
            call_id,
            tokens,
        }) = projection.tokens
        {
            self.transcript_view
                .record_tool_output_tokens(item_id, call_id, tokens);
        }
        self.set_code_mode_cell(tui, projection.block);
        if let Some(wait) = projection.wait {
            self.set_code_mode_cell(tui, wait);
        }
        tui.frame_requester().schedule_frame();
        self.chat_widget.request_pending_usage_output_insertion();
    }

    pub(super) fn insert_code_mode_cell(
        &mut self,
        tui: &mut tui::Tui,
        incoming: &Arc<dyn HistoryCell>,
    ) {
        let Some(block) = incoming
            .as_any()
            .downcast_ref::<history_cell::CodeModeCell>()
        else {
            return;
        };
        let display = self
            .chat_widget
            .thread_id()
            .or_else(|| block.owner().map(|owner| owner.thread_id));
        let Some(display) = display else {
            return;
        };
        match self.code_mode_transcript.hydrate(display, block) {
            Ok(cell) => self.set_code_mode_cell(tui, cell),
            Err(error) => tracing::warn!(%error, "rejecting conflicting Code Mode hydration"),
        }
    }

    pub(super) fn admit_code_mode_page_cell(
        &mut self,
        tui: &mut tui::Tui,
        incoming: Arc<dyn HistoryCell>,
        page: &mut Vec<Arc<dyn HistoryCell>>,
    ) {
        if incoming.as_any().is::<history_cell::CodeModeWaitCell>() {
            if self
                .transcript_cells
                .iter()
                .any(|cell| cell.activity_ids() == incoming.activity_ids())
            {
                self.set_code_mode_cell(tui, incoming);
                return;
            }
            if let Some(index) = page
                .iter()
                .position(|cell| cell.activity_ids() == incoming.activity_ids())
            {
                if !page[index]
                    .as_any()
                    .downcast_ref::<history_cell::CodeModeWaitCell>()
                    .is_some_and(history_cell::CodeModeWaitCell::completed)
                {
                    page[index] = incoming;
                }
            } else {
                page.push(incoming);
            }
            return;
        }
        let Some(block) = incoming
            .as_any()
            .downcast_ref::<history_cell::CodeModeCell>()
        else {
            return;
        };
        let Some(display) = self.chat_widget.thread_id() else {
            return;
        };
        let canonical = match self.code_mode_transcript.hydrate(display, block) {
            Ok(cell) => cell,
            Err(error) => {
                tracing::warn!(%error, "rejecting conflicting owned history page");
                return;
            }
        };
        if let Some(index) = self.transcript_cells.iter().position(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::CodeModeCell>()
                .is_some_and(|old| old.identity() == block.identity())
        }) {
            // The older page holds the block's start, so the block moves there with all it
            // knows. Existing reading/selection snapshots retain the old source revision.
            self.transcript_view.replace_group_members(
                &self.transcript_cells,
                index..index + 1,
                &[],
            );
            if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                overlay.regroup_cell_members(index..index + 1, &[]);
            }
            self.transcript_cells.remove(index);
        }
        if let Some(index) = page.iter().position(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::CodeModeCell>()
                .is_some_and(|old| old.identity() == block.identity())
        }) {
            page[index] = canonical;
        } else {
            page.push(canonical);
        }
    }

    /// Revise a block or wait in place; a finished block keeps its place ahead of its waits.
    fn set_code_mode_cell(&mut self, tui: &mut tui::Tui, incoming: Arc<dyn HistoryCell>) {
        let Some(identity) = incoming.activity_ids().first().cloned() else {
            return;
        };
        let existing = self
            .transcript_cells
            .iter()
            .position(|cell| cell.activity_ids().first() == Some(&identity));
        if let Some(index) = existing {
            if self.transcript_cells[index]
                .as_any()
                .downcast_ref::<history_cell::CodeModeWaitCell>()
                .is_some_and(history_cell::CodeModeWaitCell::completed)
            {
                return;
            }
            self.commit_transcript_splice(
                tui,
                TranscriptSplice::Members {
                    range: index..index + 1,
                    cells: vec![incoming],
                    retained: Vec::new(),
                },
            );
        } else {
            self.transcript_cells.push(incoming);
            tui.frame_requester().schedule_frame();
        }
        self.native_history.retain(&self.transcript_cells);
    }
}
