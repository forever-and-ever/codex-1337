//! App-owned transitions and draft recovery for ordered submission lanes.

use super::*;

enum RecoveryNotice {
    Rejected(String),
    Warning(String),
}
impl App {
    pub(in crate::app) fn note_submission_notification(
        &mut self,
        notification: &ServerNotification,
    ) {
        let ServerNotification::ModelSafetyBufferingUpdated(notification) =
            notification.native_notification()
        else {
            return;
        };
        let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) else {
            return;
        };
        if let Some(lane) = self.turn_submissions.lanes.get_mut(&thread_id) {
            lane.active.buffering = Some(notification.clone());
        }
    }
    pub(in crate::app) fn refresh_submission_preview(&mut self) {
        let preview = self.turn_submissions.preview(self.chat_widget.thread_id());
        self.chat_widget.update_submission_preview(preview);
    }

    pub(in crate::app) async fn enqueue_user_turn(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        op: AppCommand,
        params: TurnStartParams,
    ) -> Result<()> {
        let replay = UserTurnReplay::new(op)?;
        let op = replay.command();
        let draft = if self.chat_widget.thread_id() == Some(thread_id) {
            self.chat_widget.submission_draft(op)
        } else if let Some(input) = self.agents_overview.input_states.get(&thread_id) {
            input.submission_draft(op)
        } else if let Some(channel) = self.thread_event_channels.get(&thread_id) {
            channel.store.lock().await.input_state.as_ref().map_or_else(
                || ChatWidget::submission_draft_from_op(op),
                |input| input.submission_draft(op),
            )
        } else {
            ChatWidget::submission_draft_from_op(op)
        };
        let mut turn = ReadyTurn {
            origin: TurnOrigin::User(Box::new(replay)),
            params,
            draft,
        };
        if let TurnOrigin::User(replay) = &turn.origin
            && let Some(previous) = replay.replacement()
            && let Some(lane) = self.turn_submissions.lanes.get_mut(&thread_id)
            && let Some(waiting) =
                lane.waiting
                    .iter_mut()
                    .find_map(|entry| match &mut entry.request {
                        SubmissionRequest::Turn(turn)
                            if turn.draft.client_id == previous.client_user_message_id =>
                        {
                            Some(turn)
                        }
                        _ => None,
                    })
        {
            // The previous steer has not left this frontend. Replace it at its original FIFO slot.
            turn.params.input = previous.input.clone();
            turn.draft = waiting.draft.clone().merge_waiting(turn.draft);
            turn.params.client_user_message_id = Some(turn.draft.client_id.clone());
            if let TurnOrigin::User(replay) = &mut turn.origin {
                replay.replace_waiting(&turn.draft, &turn.params.input);
            }
            **waiting = turn;
            if self.chat_widget.thread_id() == Some(thread_id) {
                self.chat_widget
                    .finish_pending_steer_replacement(/*replaced*/ true);
            }
            self.refresh_submission_preview();
            return Ok(());
        }
        self.enqueue_submission(server, thread_id, SubmissionRequest::Turn(Box::new(turn)));
        Ok(())
    }

    pub(in crate::app) fn enqueue_subagent_turn(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        expected_turn_id: Option<String>,
        input: Vec<codex_app_server_protocol::UserInput>,
    ) {
        let client_id = Uuid::new_v4().to_string();
        let draft = ChatWidget::submission_draft_from_inputs(client_id.clone(), &input);
        let turn = ReadyTurn {
            origin: TurnOrigin::Subagent { expected_turn_id },
            params: TurnStartParams {
                thread_id: thread_id.to_string(),
                client_user_message_id: Some(client_id),
                input,
                ..Default::default()
            },
            draft,
        };
        self.enqueue_submission(server, thread_id, SubmissionRequest::Turn(Box::new(turn)));
    }

    pub(in crate::app) async fn enqueue_steer_cancellation(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        client_id: String,
        action: PendingSteerCancelAction,
    ) {
        if self.turn_submissions.lanes.get(&thread_id).is_some_and(|lane|
            std::iter::once(&lane.active).chain(&lane.waiting).any(|entry|
                matches!(&entry.request, SubmissionRequest::Cancel { client_id: pending, .. } if pending == &client_id)))
        {
            self.chat_widget.add_info_message("Pending steer cancellation is already in progress.".into(), /*hint*/ None);
            return;
        }
        let waiting = self
            .turn_submissions
            .lanes
            .get(&thread_id)
            .and_then(|lane| {
                lane.waiting
                    .iter()
                    .find(|entry| {
                        matches!(entry.request, SubmissionRequest::Turn(_))
                            && entry.client_id() == client_id
                    })
                    .map(|entry| entry.id)
            });
        if let Some(id) = waiting {
            self.handle_submission_event(SubmissionEvent::Edit {
                thread_id,
                id,
                action,
            })
            .await;
        } else {
            let draft = if self.chat_widget.thread_id() == Some(thread_id) {
                self.chat_widget.cancellation_draft(&client_id)
            } else if let Some(input) = self.agents_overview.input_states.get(&thread_id) {
                input.cancellation_draft(&client_id)
            } else if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                channel
                    .store
                    .lock()
                    .await
                    .input_state
                    .as_ref()
                    .and_then(|input| input.cancellation_draft(&client_id))
            } else {
                None
            };
            self.enqueue_submission(
                server,
                thread_id,
                SubmissionRequest::Cancel {
                    client_id,
                    action,
                    draft: draft.map(Box::new),
                },
            );
        }
    }

    fn enqueue_submission(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        request: SubmissionRequest,
    ) {
        let entry = PendingSubmission {
            id: Uuid::new_v4(),
            request,
            buffering: None,
        };
        if let Some(lane) = self.turn_submissions.lanes.get_mut(&thread_id) {
            lane.waiting.push_back(entry);
        } else {
            self.start_submission(server, thread_id, entry, VecDeque::new());
        }
        self.refresh_submission_preview();
    }

    fn start_submission(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        active: PendingSubmission,
        waiting: VecDeque<PendingSubmission>,
    ) {
        let id = active.id;
        let request = active.worker_request();
        let handle = server.request_handle();
        let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
        let completions = self.thread_delivery.completion_tx.clone();
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let outcome = worker::send(handle, thread_id, request, store).await;
            let _ = completions
                .send(LocalThreadCompletion::Submission {
                    thread_id,
                    id,
                    outcome,
                })
                .await;
        }));
        self.turn_submissions.lanes.insert(
            thread_id,
            SubmissionLane {
                active,
                task,
                waiting,
            },
        );
    }

    pub(in crate::app) async fn handle_submission_event(&mut self, event: SubmissionEvent) {
        match event {
            SubmissionEvent::Edit {
                thread_id,
                id,
                action,
            } => {
                let entry = self
                    .turn_submissions
                    .lanes
                    .get_mut(&thread_id)
                    .and_then(|lane| {
                        let index = lane.waiting.iter().position(|entry| {
                            entry.id == id && matches!(entry.request, SubmissionRequest::Turn(_))
                        })?;
                        lane.waiting.remove(index)
                    });
                if let Some(draft) = entry.and_then(PendingSubmission::into_draft) {
                    self.restore_submission_drafts(thread_id, vec![draft], action.into())
                        .await;
                } else {
                    self.chat_widget.add_info_message(
                        "Message is already sending or sent; it cannot be edited here.".into(),
                        /*hint*/ None,
                    );
                }
            }
            SubmissionEvent::Retire => {
                let lanes = std::mem::take(&mut self.turn_submissions.lanes);
                if !lanes.is_empty() {
                    self.chat_widget.add_error_message("Connection changed while sending. The original may still be delivered; pending messages return as drafts and have not been resent.".into());
                }
                for (thread_id, lane) in lanes {
                    lane.task.abort();
                    let uncertain_id = Some(lane.active.client_id().to_owned());
                    let drafts =
                        recoverable_drafts(std::iter::once(lane.active).chain(lane.waiting));
                    self.restore_submission_drafts(
                        thread_id,
                        drafts,
                        DraftDisposition::Recover { uncertain_id },
                    )
                    .await;
                }
            }
        }
        self.refresh_submission_preview();
    }

    pub(in crate::app) async fn finish_submission(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        id: Uuid,
        outcome: SubmissionOutcome,
    ) -> Result<()> {
        if self.reconnect.offline {
            self.handle_submission_event(SubmissionEvent::Retire).await;
            return Ok(());
        }
        let std::collections::hash_map::Entry::Occupied(entry) =
            self.turn_submissions.lanes.entry(thread_id)
        else {
            return Ok(());
        };
        if entry.get().active.id != id {
            return Ok(());
        }
        let SubmissionLane {
            active,
            task,
            mut waiting,
        } = entry.remove();
        drop(task);
        let current = self.chat_widget.thread_id() == Some(thread_id);
        let mut transport_failed = false;
        let uncertain_id = match &outcome {
            SubmissionOutcome::Finished {
                result: Err(SubmissionFailure::Uncertain(_) | SubmissionFailure::TimedOut),
                ..
            } => Some(active.client_id().to_owned()),
            _ => None,
        };
        let failure = match outcome {
            SubmissionOutcome::Replaced => {
                self.finish_submission_replacement(thread_id, &active, true)
                    .await;
                None
            }
            SubmissionOutcome::Finished {
                replacement,
                observations,
                result,
            } => {
                if matches!(replacement, ReplacementContinuation::Missed) {
                    self.finish_submission_replacement(thread_id, &active, false)
                        .await;
                }
                if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                    let mut store = channel.store.lock().await;
                    let latest_before_observations = store.latest_turn_id.clone();
                    for observation in observations {
                        match observation {
                            TurnObservation::Missing { expected }
                                if store.active_turn_id() == Some(expected.as_str()) =>
                            {
                                store.clear_active_turn_id()
                            }
                            TurnObservation::Mismatch { expected, actual }
                                if store.active_turn_id() == Some(expected.as_str()) =>
                            {
                                store.set_active_turn_id(actual)
                            }
                            _ => {}
                        }
                    }
                    if let Ok(SubmissionReceipt::Started {
                        turn_id,
                        previous_latest,
                    }) = &result
                        && &latest_before_observations == previous_latest
                    {
                        store.set_active_turn_id(turn_id.clone());
                    }
                }
                match result {
                    Ok(SubmissionReceipt::Started { turn_id, .. }) => {
                        if current && let SubmissionRequest::Turn(turn) = &active.request
                            && let TurnOrigin::User(replay) = &turn.origin {
                            self.chat_widget.record_submission_receipt(turn_id, replay.command(), active.buffering.as_ref());
                        }
                        None
                    }
                    Ok(SubmissionReceipt::Steered) => None,
                    Ok(SubmissionReceipt::Cancelled(cancelled)) => {
                        if let SubmissionRequest::Cancel { client_id, action, .. } = &active.request {
                            self.finish_submission_cancel(thread_id, client_id, *action, cancelled).await;
                        }
                        None
                    }
                    Ok(SubmissionReceipt::Deferred(error)) => Some(RecoveryNotice::Warning(error)),
                    Err(SubmissionFailure::Rejected(error)) => {
                        let message = match &error {
                            TypedRequestError::Server { method, .. } if method == "turn/start" =>
                                format!("Failed to start turn: turn/start failed in TUI: {error}"),
                            _ => format!("Submission rejected: {error}"),
                        };
                        Some(RecoveryNotice::Rejected(message))
                    },
                    Err(SubmissionFailure::Uncertain(error)) => {
                        transport_failed = matches!(error, TypedRequestError::Transport { .. });
                        Some(RecoveryNotice::Warning(format!("{error}. The original may still be delivered; check before resending.")))
                    }
                    Err(SubmissionFailure::TimedOut) => Some(RecoveryNotice::Warning("Submission acknowledgement timed out. The original may still be delivered; check before resending.".into())),
                }
            }
        };
        if let Some(error) = failure {
            let drafts = recoverable_drafts(std::iter::once(active).chain(waiting));
            self.restore_submission_drafts(
                thread_id,
                drafts,
                DraftDisposition::Recover { uncertain_id },
            )
            .await;
            match error {
                RecoveryNotice::Rejected(message) => self.chat_widget.add_error_message(message),
                RecoveryNotice::Warning(message) => self.chat_widget.add_error_message(format!(
                    "{message} Pending messages were recovered as drafts and have not been resent."
                )),
            }
            if transport_failed {
                self.begin_reconnect();
            }
        } else {
            if current
                && let SubmissionRequest::Turn(turn) = &active.request
                && matches!(turn.origin, TurnOrigin::User(_))
            {
                crate::startup_recovery::acknowledged(active.client_id());
            }
            if let Some(next) = waiting.pop_front() {
                self.start_submission(server, thread_id, next, waiting);
            }
        }
        self.refresh_submission_preview();
        Ok(())
    }

    async fn restore_submission_drafts(
        &mut self,
        thread_id: ThreadId,
        drafts: Vec<SubmissionDraft>,
        action: DraftDisposition,
    ) {
        if drafts.is_empty() {
            return;
        }
        if self.chat_widget.thread_id() == Some(thread_id) {
            self.chat_widget.recover_submission_drafts(drafts, action);
        } else if let Some(input) = self.agents_overview.input_states.get_mut(&thread_id) {
            input.recover_submission_drafts(drafts, action);
        } else if let Some(channel) = self.thread_event_channels.get(&thread_id)
            && let Some(input) = channel.store.lock().await.input_state.as_mut()
        {
            input.recover_submission_drafts(drafts, action);
        } else {
            self.chat_widget.recover_submission_drafts(drafts, action);
            self.chat_widget.add_warning_message(format!("The original thread {thread_id} is no longer open. Its unsent messages were recovered into this draft."));
        }
    }

    async fn finish_submission_replacement(
        &mut self,
        thread_id: ThreadId,
        submission: &PendingSubmission,
        replaced: bool,
    ) {
        let SubmissionRequest::Turn(turn) = &submission.request else {
            return;
        };
        let draft = &turn.draft;
        if self.chat_widget.thread_id() == Some(thread_id) {
            self.chat_widget
                .finish_submission_replacement(&draft.client_id, replaced);
        } else if let Some(input) = self.agents_overview.input_states.get_mut(&thread_id) {
            input.finish_submission_replacement(draft, replaced);
        } else if let Some(channel) = self.thread_event_channels.get(&thread_id)
            && let Some(input) = channel.store.lock().await.input_state.as_mut()
        {
            input.finish_submission_replacement(draft, replaced);
        }
    }
    async fn finish_submission_cancel(
        &mut self,
        thread_id: ThreadId,
        client_id: &str,
        action: PendingSteerCancelAction,
        cancelled: bool,
    ) {
        if self.chat_widget.thread_id() == Some(thread_id) {
            self.chat_widget
                .finish_submission_cancel(client_id, action, cancelled);
        } else if let Some(input) = self.agents_overview.input_states.get_mut(&thread_id) {
            input.finish_submission_cancel(client_id, action, cancelled);
        } else if let Some(channel) = self.thread_event_channels.get(&thread_id)
            && let Some(input) = channel.store.lock().await.input_state.as_mut()
        {
            input.finish_submission_cancel(client_id, action, cancelled);
        }
    }

    /// Test drivers, like the real App loop, must consume worker receipts as well as notifications.
    #[cfg(test)]
    pub(in crate::app) async fn settle_turn_submissions(
        &mut self,
        server: &mut AppServerSession,
    ) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.turn_submissions.is_empty() {
                let completion = self
                    .thread_delivery
                    .completion_rx
                    .recv()
                    .await
                    .ok_or_else(|| color_eyre::eyre::eyre!("submission receipt channel closed"))?;
                self.handle_local_thread_completion(server, completion)
                    .await?;
            }
            Ok::<_, color_eyre::Report>(())
        })
        .await??;
        Ok(())
    }
}
