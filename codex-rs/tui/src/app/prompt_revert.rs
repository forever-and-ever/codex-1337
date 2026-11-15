//! Prompt-edit custody across history reads, mutation acknowledgment, and normal-pump replay.

use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::Arc;
use super::ClientRequest;
use super::Duration;
use super::HistoryCell;
use super::Instant;
use super::Mutex;
use super::ReplayKind;
use super::Result;
use super::ServerNotification;
use super::ThreadBufferedEvent;
use super::ThreadEventStore;
use super::ThreadId;
use super::Turn;
use super::TypedRequestError;
use super::Uuid;
use super::tui;
use super::turn_submissions;
use super::turn_submissions::DraftDisposition;
use super::turn_submissions::DraftOrigin;
use super::turn_submissions::SubmissionDraft;
use super::turn_submissions::ui_requests::UiRequestHandle;
use super::turn_submissions::ui_requests::UiRequestKind;
use super::turn_submissions::ui_requests::UiRequestScope;
use crate::chatwidget::UserMessage;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadRevertParams;
use codex_app_server_protocol::ThreadRevertResponse;
use tokio::sync::oneshot;

pub(super) struct PendingRevert {
    id: Uuid,
    thread_id: ThreadId,
    epoch: u64,
    store: Arc<Mutex<ThreadEventStore>>,
    selected_cell: Arc<dyn HistoryCell>,
    latest_turn_id: Option<String>,
    prompt: Option<UserMessage>,
    phase: RevertPhase,
}

enum RevertPhase {
    Reading(UiRequestHandle),
    Mutating {
        notification: Option<oneshot::Sender<()>>,
        retained_turns: Vec<Turn>,
    },
    Uncertain(String),
}

pub(crate) struct RevertSelection {
    prompt: UserMessage,
    observation: crate::app_server_session::history_job::HistoryObservation,
    selection: Result<(String, Vec<Turn>), String>,
}

impl std::fmt::Debug for RevertSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RevertSelection")
    }
}

#[derive(Debug)]
pub(crate) struct RevertFailure {
    uncertain: bool,
    message: String,
}

impl App {
    pub(super) async fn start_prompt_revert(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        selected_cell: Arc<dyn HistoryCell>,
        prompt: UserMessage,
    ) {
        if self.current_displayed_thread_id() != Some(thread_id) {
            return;
        }
        if self.pending_revert.is_some() {
            self.restore_backtrack_prompt_after_revert_error(
                prompt,
                "another prompt edit is still pending",
            );
            return;
        }
        if self.app_server_target.uses_remote_workspace()
            && (!prompt.local_images.is_empty() || prompt.text.trim_start().starts_with(['/', '!']))
        {
            self.chat_widget.add_error_message("This remote prompt contains local image paths or command syntax that cannot be restored safely. Write a new message and reattach any images.".into());
            return;
        }
        if self.pending_server_profiles.contains_key(&thread_id) {
            self.restore_backtrack_prompt_after_revert_error(
                prompt,
                "wait for permissions to update",
            );
            return;
        }
        let Some(index) = self
            .transcript_cells
            .iter()
            .position(|cell| Arc::ptr_eq(cell, &selected_cell))
        else {
            self.restore_backtrack_prompt_after_revert_error(
                prompt,
                "the selected prompt is no longer visible",
            );
            return;
        };
        let Some(channel) = self.thread_event_channels.get(&thread_id) else {
            self.restore_backtrack_prompt_after_revert_error(
                prompt,
                "the selected thread is no longer available",
            );
            return;
        };
        let store = Arc::clone(&channel.store);
        let (start_item, loaded_tail, latest_turn_id) = {
            let mut store = store.lock().await;
            store.input_state = self.chat_widget.capture_thread_input_state();
            (
                store.turns.iter().find_map(|turn| {
                    turn.items
                        .first()
                        .map(|item| (turn.id.clone(), item.id().to_string()))
                }),
                store.turns.last().map(|turn| turn.id.clone()),
                store.latest_turn_id.clone(),
            )
        };
        let nth_user_message = crate::app_backtrack::user_count(&self.transcript_cells[..index]);
        let id = Uuid::new_v4();
        self.chat_widget
            .set_prompt_revert_notice(Some("Reverting…"));
        let original_prompt = prompt.clone();
        let latest = latest_turn_id.clone();
        let image_origin = if self.app_server_target.uses_remote_workspace() {
            crate::history_cell::ImagePathOrigin::Remote
        } else {
            crate::history_cell::ImagePathOrigin::Local
        };
        let handle = self.request_server(server, UiRequestScope::Thread(thread_id), UiRequestKind::ReadOnly, "load prompt edit history",
            move |handle| async move {
                let mut prompt = prompt;
                let through = start_item.as_ref().map(|(turn, _)| turn).or(loaded_tail.as_ref());
                let prepared = crate::app_server_session::history_job::load(&handle, thread_id, through.map(String::as_str), None, &|| {}, image_origin).await;
                let selection = prepared.result.map_err(|error| error.to_string()).and_then(|mut thread| {
                    if thread.turns.last().map(|turn| &turn.id) != latest.as_ref() {
                        return Err("thread history changed; reload the session before editing this prompt".into());
                    }
                    let start_item = start_item.or_else(|| loaded_tail.as_ref().and_then(|tail| {
                        thread.turns.iter().position(|turn| &turn.id == tail).and_then(|index| {
                            thread.turns[index + 1..].iter().find_map(|turn| turn.items.first().map(|item| (turn.id.clone(), item.id().to_string())))
                        })
                    }));
                    let before = crate::app_backtrack::backtrack_revert_before_turn_id(&thread.turns, start_item.as_ref(), nth_user_message, &mut prompt).map_err(|error| error.to_string())?;
                    let cut = thread.turns.iter().position(|turn| turn.id == before).ok_or("selected turn disappeared")?;
                    thread.turns.truncate(cut);
                    if let Some((turn_id, item_id)) = &start_item {
                        for turn in &mut thread.turns {
                            if &turn.id == turn_id {
                                if let Some(index) = turn.items.iter().position(|item| item.id() == item_id) { turn.items.drain(..index); }
                                break;
                            }
                            turn.items.clear();
                        }
                    }
                    Ok((before, thread.turns))
                });
                Ok(RevertSelection { prompt, selection, observation: prepared.observation })
            },
            move |app, _, result| {
                app.app_event_tx.send(AppEvent::PromptRevertPrepared { id, result: result.map_err(ToString::to_string) });
                Ok(())
            });
        self.pending_revert = Some(PendingRevert {
            id,
            thread_id,
            epoch: self.server_operations.epoch(),
            store,
            selected_cell,
            latest_turn_id,
            prompt: Some(original_prompt),
            phase: RevertPhase::Reading(handle),
        });
    }

    #[expect(
        clippy::expect_used,
        reason = "The matching pending prompt edit was checked before taking it."
    )]
    pub(super) async fn prompt_revert_prepared(
        &mut self,
        server: &mut AppServerSession,
        id: Uuid,
        result: Result<RevertSelection, String>,
    ) {
        if self
            .pending_revert
            .as_ref()
            .is_none_or(|pending| pending.id != id)
        {
            return;
        }
        let mut pending = self.pending_revert.take().expect("matched prompt edit");
        let selection = match result {
            Ok(result) => {
                if pending.epoch == self.server_operations.epoch() {
                    result.observation.apply(server);
                }
                pending.prompt = Some(result.prompt);
                result.selection
            }
            Err(error) => Err(error),
        };
        let source_matches = self.current_displayed_thread_id() == Some(pending.thread_id)
            && self.server_operations.epoch() == pending.epoch
            && self
                .thread_event_channels
                .get(&pending.thread_id)
                .is_some_and(|channel| Arc::ptr_eq(&channel.store, &pending.store))
            && pending.store.lock().await.latest_turn_id == pending.latest_turn_id
            && self
                .transcript_cells
                .iter()
                .any(|cell| Arc::ptr_eq(cell, &pending.selected_cell));
        let selection = if source_matches {
            selection
        } else {
            Err("prompt edit canceled because its source changed".into())
        };
        let (before_turn_id, retained_turns) = match selection {
            Ok(selection) => selection,
            Err(error) => {
                self.recover_prompt_revert(&mut pending).await;
                if self.chat_widget.thread_id() == Some(pending.thread_id) {
                    self.chat_widget.set_prompt_revert_notice(None);
                    self.chat_widget
                        .add_error_message(format!("Failed to edit the selected prompt: {error}"));
                }
                return;
            }
        };
        let (tx, rx) = oneshot::channel();
        let thread_id = pending.thread_id;
        pending.phase = RevertPhase::Mutating {
            notification: Some(tx),
            retained_turns,
        };
        self.pending_revert = Some(pending);
        self.request_server(server, UiRequestScope::Thread(thread_id), UiRequestKind::Mutation, "revert prompt",
            move |handle| async move {
                let response: ThreadRevertResponse = handle.request_typed(ClientRequest::ThreadRevert {
                    request_id: RequestId::String(format!("tui-prompt-revert-{id}")),
                    params: ThreadRevertParams { thread_id: thread_id.to_string(), before_turn_id },
                }).await.map_err(|error| {
                    // Only pre-mutation validation errors prove history is unchanged.
                    if matches!(&error, TypedRequestError::Server { source, .. } if !matches!(source.code, -32602..=-32600)) {
                        revert_transport_error(std::io::ErrorKind::Other, error.to_string())
                    } else { error }
                })?;
                tokio::time::timeout(Duration::from_secs(10), rx).await
                    .map_err(|_| revert_transport_error(std::io::ErrorKind::TimedOut, "history revert acknowledged, but its notification did not arrive"))?
                    .map_err(|_| revert_transport_error(std::io::ErrorKind::BrokenPipe, "history revert observer disconnected"))?;
                Ok(response)
            },
            move |app, _, result| {
                let result = result.map_err(|failure| RevertFailure {
                    uncertain: !matches!(failure, turn_submissions::SubmissionFailure::Rejected(_)),
                    message: failure.to_string(),
                });
                app.app_event_tx.send(AppEvent::PromptRevertFinished { id, result });
                Ok(())
            });
    }

    pub(super) fn note_prompt_revert_notification(&mut self, notification: &ServerNotification) {
        if let ServerNotification::ThreadReverted(notification) = notification
            && let Some(pending) = &mut self.pending_revert
            && notification.thread_id == pending.thread_id.to_string()
            && let RevertPhase::Mutating { notification, .. } = &mut pending.phase
            && let Some(tx) = notification.take()
        {
            let _ = tx.send(());
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "Prompt edit admission retains the source input until recovery."
    )]
    async fn recover_prompt_revert(&mut self, pending: &mut PendingRevert) {
        let Some(prompt) = pending.prompt.take() else {
            return;
        };
        if self.chat_widget.thread_id() == Some(pending.thread_id) {
            self.chat_widget.restore_user_message_to_composer(prompt);
            return;
        }

        let drafts = vec![SubmissionDraft {
            client_id: Uuid::new_v4().to_string(),
            message: prompt,
            origin: DraftOrigin::Message {
                source: crate::chatwidget::UserMessageSource::Prompt,
            },
        }];
        if let Some(input) = self
            .agents_overview
            .input_states
            .get_mut(&pending.thread_id)
        {
            input.recover_submission_drafts(drafts, DraftDisposition::Edit);
        } else {
            let mut store = pending.store.lock().await;
            let input = store
                .input_state
                .as_mut()
                .expect("prompt edit retained its source input");
            input.recover_submission_drafts(drafts, DraftDisposition::Edit);
            self.agents_overview
                .input_states
                .insert(pending.thread_id, input.clone());
        }
    }

    pub(super) fn sync_prompt_revert(&mut self) {
        let Some(pending) = &self.pending_revert else {
            return;
        };
        let current = self.current_displayed_thread_id() == Some(pending.thread_id);
        let same_epoch = pending.epoch == self.server_operations.epoch();
        let same_store = self
            .thread_event_channels
            .get(&pending.thread_id)
            .is_some_and(|channel| Arc::ptr_eq(&channel.store, &pending.store));
        let stale = match &pending.phase {
            RevertPhase::Reading(handle) if !current || !same_epoch => {
                handle.cancel();
                true
            }
            RevertPhase::Mutating { .. } => !same_epoch,
            RevertPhase::Uncertain(_) => !same_store,
            RevertPhase::Reading(_) => false,
        };
        if stale {
            self.app_event_tx
                .send(AppEvent::ReconcilePromptRevert { id: pending.id });
        }
        if current && same_store {
            let notice = match &pending.phase {
                RevertPhase::Uncertain(message) => message.as_str(),
                _ => "Reverting…",
            };
            self.chat_widget.project_prompt_revert_notice(notice);
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "The matching pending prompt edit was checked before taking it."
    )]
    pub(super) async fn reconcile_prompt_revert(&mut self, id: Uuid) {
        if self
            .pending_revert
            .as_ref()
            .is_none_or(|pending| pending.id != id)
        {
            return;
        }
        let mut pending = self.pending_revert.take().expect("matched prompt edit");
        let current = self.chat_widget.thread_id() == Some(pending.thread_id);
        let same_store = self
            .thread_event_channels
            .get(&pending.thread_id)
            .is_some_and(|channel| Arc::ptr_eq(&channel.store, &pending.store));
        if let RevertPhase::Reading(handle) = &pending.phase
            && (!current || self.server_operations.epoch() != pending.epoch)
        {
            handle.cancel();
            self.recover_prompt_revert(&mut pending).await;
            if current {
                self.chat_widget.set_prompt_revert_notice(None);
            }
            return;
        }
        if matches!(pending.phase, RevertPhase::Mutating { .. })
            && self.server_operations.epoch() != pending.epoch
        {
            self.recover_prompt_revert(&mut pending).await;
            pending.phase = RevertPhase::Uncertain(
                "Revert unconfirmed after disconnect; resume this task to reload history.".into(),
            );
        }
        if matches!(pending.phase, RevertPhase::Uncertain(_)) && !same_store {
            if current {
                self.chat_widget.set_prompt_revert_notice(None);
            }
            return;
        }
        if current {
            let notice = match &pending.phase {
                RevertPhase::Uncertain(message) => message.as_str(),
                _ => "Reverting…",
            };
            self.chat_widget.set_prompt_revert_notice(Some(notice));
        }
        self.pending_revert = Some(pending);
    }

    #[expect(
        clippy::expect_used,
        reason = "The matching pending prompt edit was checked before taking it."
    )]
    pub(super) async fn finish_prompt_revert(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        id: Uuid,
        result: Result<ThreadRevertResponse, RevertFailure>,
    ) -> Result<()> {
        if self
            .pending_revert
            .as_ref()
            .is_none_or(|pending| pending.id != id)
        {
            return Ok(());
        }
        let mut pending = self.pending_revert.take().expect("matched prompt edit");
        let result = if pending.epoch == self.server_operations.epoch() {
            result
        } else {
            Err(RevertFailure {
                uncertain: true,
                message: "connection changed before prompt editing was confirmed".into(),
            })
        };
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.recover_prompt_revert(&mut pending).await;
                let current = self.chat_widget.thread_id() == Some(pending.thread_id);
                if error.uncertain {
                    let notice = "Revert unconfirmed; close and reopen this TUI to reload history.";
                    if current {
                        self.chat_widget.set_prompt_revert_notice(Some(notice));
                        self.chat_widget.add_error_message(format!(
                            "Prompt edit could not be confirmed: {}. No request was retried.",
                            error.message
                        ));
                    }
                    pending.phase = RevertPhase::Uncertain(notice.into());
                    self.pending_revert = Some(pending);
                } else if current {
                    self.chat_widget.set_prompt_revert_notice(None);
                    self.chat_widget.add_error_message(format!(
                        "Failed to edit the selected prompt: {}",
                        error.message
                    ));
                }
                return Ok(());
            }
        };
        let RevertPhase::Mutating { retained_turns, .. } =
            std::mem::replace(&mut pending.phase, RevertPhase::Uncertain(String::new()))
        else {
            return Ok(());
        };
        if !self
            .thread_event_channels
            .get(&pending.thread_id)
            .is_some_and(|channel| Arc::ptr_eq(&channel.store, &pending.store))
        {
            self.recover_prompt_revert(&mut pending).await;
            return Ok(());
        }
        server.apply_prompt_revert(pending.thread_id, &retained_turns, &response);
        let thread_id = pending.thread_id;
        let current = self.chat_widget.thread_id() == Some(thread_id);
        let session = pending.store.lock().await.session.clone();
        let Some(mut session) = session else {
            self.recover_prompt_revert(&mut pending).await;
            let notice = "Reverted history could not be displayed; reopen this task to reload it.";
            if current {
                self.chat_widget.set_prompt_revert_notice(Some(notice));
                self.chat_widget.add_error_message(notice.into());
            }
            pending.phase = RevertPhase::Uncertain(notice.into());
            self.pending_revert = Some(pending);
            return Ok(());
        };
        session.rollout_path = response.thread.path.clone();
        if !current {
            self.recover_prompt_revert(&mut pending).await;
            let mut store = pending.store.lock().await;
            store.buffer.clear();
            store
                .at_connection_epoch(self.reconnect.connection_epoch)
                .set_session(session, retained_turns);
            return Ok(());
        }
        self.chat_widget.restore_thread_input_state(
            None,
            crate::chatwidget::ThreadInputStateRestoreMode {
                preserve_in_flight_turn: false,
            },
        );
        self.recover_prompt_revert(&mut pending).await;
        self.chat_widget.set_queue_autosend_suppressed(true);
        self.abort_thread_event_listener(thread_id);
        if let Some(mut rx) = self.active_thread_rx.take() {
            while let Ok(event) = rx.try_recv() {
                if let ThreadBufferedEvent::Notification(notification) = event {
                    self.chat_widget.handle_server_notification(
                        *notification,
                        Some(ReplayKind::ThreadSnapshot),
                    );
                }
            }
        }
        if self.primary_thread_id == Some(thread_id) {
            self.primary_session_configured = Some(session.clone());
        }
        self.thread_event_channels.remove(&thread_id);
        self.active_thread_id = None;
        self.recap.reset_for_new_thread(Instant::now());
        self.recap.seed_from_turns(&retained_turns, Instant::now());
        self.retain_realtime_replay_state_before_replace();
        self.forget_realtime_replay_thread(thread_id);
        self.chat_widget
            .reset_after_prompt_revert(response.thread.path, &retained_turns);
        self.ensure_thread_channel(thread_id)
            .store
            .lock()
            .await
            .set_session(session, retained_turns);
        self.activate_thread_channel(thread_id).await;
        self.chat_widget.set_prompt_revert_notice(None);
        self.chat_widget.set_queue_autosend_suppressed(true);
        self.pending_thread_switch_resets += 1;
        self.app_event_tx.send(AppEvent::FinishPromptRevert {
            thread_id,
            selected_cell: pending.selected_cell,
        });
        tui.frame_requester().schedule_frame();
        Ok(())
    }
}

fn revert_transport_error(
    kind: std::io::ErrorKind,
    message: impl Into<String>,
) -> TypedRequestError {
    TypedRequestError::Transport {
        method: "thread/revert".into(),
        source: std::io::Error::new(kind, message.into()),
    }
}
