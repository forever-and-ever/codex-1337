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
        if let Some(lane) = self
            .server_operations
            .lanes
            .get_mut(&OperationLane::Thread(thread_id))
        {
            lane.active.buffering = Some(notification.clone());
        }
    }
    pub(in crate::app) fn refresh_submission_preview(&mut self) {
        let preview = self.server_operations.preview(self.chat_widget.thread_id());
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
            && let Some(lane) = self
                .server_operations
                .lanes
                .get_mut(&OperationLane::Thread(thread_id))
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
        if self.server_operations.lanes.get(&OperationLane::Thread(thread_id)).is_some_and(|lane|
            std::iter::once(&lane.active).chain(&lane.waiting).any(|entry|
                matches!(&entry.request, SubmissionRequest::Cancel { client_id: pending, .. } if pending == &client_id)))
        {
            self.chat_widget.add_info_message("Pending steer cancellation is already in progress.".into(), /*hint*/ None);
            return;
        }
        let waiting = self
            .server_operations
            .lanes
            .get(&OperationLane::Thread(thread_id))
            .and_then(|lane| {
                lane.waiting
                    .iter()
                    .find(|entry| {
                        matches!(entry.request, SubmissionRequest::Turn(_))
                            && entry.client_id() == Some(client_id.as_str())
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

    pub(in crate::app) fn enqueue_misalignment_continuation(
        &mut self,
        server: &AppServerSession,
        review: Arc<crate::chatwidget::MisalignmentReview>,
        params: TurnStartParams,
    ) {
        let thread_id = review.thread_id;
        self.enqueue_submission(
            server,
            thread_id,
            SubmissionRequest::Continuation {
                review,
                params: Box::new(params),
            },
        );
    }

    async fn finish_misalignment_continuation(
        &mut self,
        thread_id: ThreadId,
        submission: &PendingSubmission,
        turn: codex_app_server_protocol::Turn,
    ) {
        let SubmissionRequest::Continuation { review, .. } = &submission.request else {
            unreachable!("continuation receipt without reviewed intent");
        };
        let mut store = submission.store.lock().await;
        if store
            .latest_turn_id
            .as_deref()
            .is_some_and(|latest| latest != review.turn_id && latest != turn.id)
        {
            return;
        }
        let observed = store.latest_turn_id.as_deref() == Some(turn.id.as_str());
        if !observed {
            store.set_active_turn_id(turn.id.clone());
        }
        drop(store);
        if self.chat_widget.thread_id() == Some(thread_id)
            && self.chat_widget.is_current_misalignment_review(review)
        {
            self.chat_widget.clear_misalignment_for_new_turn(
                &turn.id,
                crate::chatwidget::MisalignmentTurnSource::AcknowledgedContinuation,
            );
            if !observed {
                self.chat_widget.handle_server_notification(
                    ServerNotification::TurnStarted(
                        codex_app_server_protocol::TurnStartedNotification {
                            thread_id: thread_id.to_string(),
                            turn,
                        },
                    ),
                    None,
                );
            }
        }
    }

    pub(super) fn enqueue_submission(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        request: SubmissionRequest,
    ) {
        let key = match &request {
            SubmissionRequest::Operation(operation) => operation.lane(thread_id),
            _ => OperationLane::Thread(thread_id),
        };
        let order = self.server_operations.next_order;
        self.server_operations.next_order =
            order.checked_add(1).expect("operation sequence exhausted");
        let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
        let entry = PendingSubmission {
            id: Uuid::new_v4(),
            order,
            epoch: self.server_operations.epoch,
            thread_id,
            store,
            request,
            buffering: None,
        };
        if let Some(lane) = self.server_operations.lanes.get_mut(&key) {
            lane.waiting.push_back(entry);
        } else {
            self.server_operations.lanes.insert(
                key,
                SubmissionLane {
                    active: entry,
                    state: LaneState::Queued,
                    waiting: VecDeque::new(),
                },
            );
        }
        self.drive_server_operations(server);
        self.refresh_submission_preview();
    }

    pub(in crate::app) fn drive_server_operations(&mut self, server: &AppServerSession) {
        let barrier = self
            .server_operations
            .lanes
            .get(&OperationLane::Connection)
            .map(|lane| lane.active.order);
        let ready: Vec<_> = self
            .server_operations
            .lanes
            .iter()
            .filter_map(|(key, lane)| {
                if !matches!(lane.state, LaneState::Queued) {
                    return None;
                }
                let ready = match key {
                    OperationLane::Thread(_) => {
                        barrier.is_none_or(|order| lane.active.order < order)
                    }
                    OperationLane::Connection => {
                        !self
                            .server_operations
                            .lanes
                            .iter()
                            .any(|(other, predecessor)| {
                                *other != OperationLane::Connection
                                    && predecessor.active.order < lane.active.order
                            })
                    }
                };
                ready.then_some(*key)
            })
            .collect();
        for key in ready {
            let lane = self
                .server_operations
                .lanes
                .remove(&key)
                .expect("queued operation lane disappeared");
            self.start_submission(server, lane.active.thread_id, lane.active, lane.waiting);
        }
    }

    fn start_submission(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        active: PendingSubmission,
        waiting: VecDeque<PendingSubmission>,
    ) {
        let id = active.id;
        let epoch = active.epoch;
        let request = active.worker_request();
        let handle = server.request_handle();
        let store = Arc::clone(&active.store);
        let completions = self.thread_delivery.completion_tx.clone();
        let settings_supported = server.operation_settings_supported();
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let outcome = if !settings_supported
                && matches!(&request, WorkerRequest::Operation { operation, .. } if matches!(&**operation, ThreadOperation::Mutation(requests::ThreadMutation::Settings(_))))
            {
                SubmissionOutcome::Operation(Ok(OperationReceipt::SettingsUnsupported))
            } else {
                worker::send(handle, thread_id, request, store).await
            };
            let _ = completions
                .send(LocalThreadCompletion::Submission {
                    thread_id,
                    id,
                    epoch,
                    outcome,
                })
                .await;
        }));
        let key = match &active.request {
            SubmissionRequest::Operation(operation) => operation.lane(thread_id),
            _ => OperationLane::Thread(thread_id),
        };
        self.server_operations.lanes.insert(
            key,
            SubmissionLane {
                active,
                state: LaneState::Running(task),
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
                let entry = self.server_operations.take_unsent(thread_id, id, action);
                if let Some(entry) = entry {
                    let origin = entry.thread_id;
                    if let SubmissionRequest::Operation(operation) = &entry.request {
                        self.chat_widget.add_info_message(
                            format!("{} cancelled before sending.", operation.label()),
                            None,
                        );
                        if self.chat_widget.thread_id() == Some(origin)
                            && matches!(
                                &**operation,
                                ThreadOperation::Mutation(requests::ThreadMutation::DisarmAlarm(_))
                            )
                        {
                            self.chat_widget.alarm_disarm_failed(
                                "Alarm disarm was cancelled before sending.".into(),
                            );
                        }
                    }
                    if let Some(draft) = entry.into_draft() {
                        self.restore_submission_drafts(origin, vec![draft], action.into())
                            .await;
                    }
                } else {
                    self.chat_widget.add_info_message(
                        "Message is already sending or sent; it cannot be edited here.".into(),
                        /*hint*/ None,
                    );
                }
            }
            SubmissionEvent::Retire => {
                self.server_operations.epoch = self
                    .server_operations
                    .epoch
                    .checked_add(1)
                    .expect("operation epoch exhausted");
                let lanes = std::mem::take(&mut self.server_operations.lanes);
                if !lanes.is_empty() {
                    self.chat_widget.add_error_message("Connection changed while sending. The original may still be delivered; pending messages return as drafts and have not been resent.".into());
                }
                for (_, lane) in lanes {
                    let thread_id = lane.active.thread_id;
                    let uncertain_id = (!matches!(lane.state, LaneState::Queued))
                        .then(|| lane.active.client_id().map(str::to_owned))
                        .flatten();
                    if let LaneState::Running(task) = lane.state {
                        task.abort();
                    }
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
        server: &mut AppServerSession,
        thread_id: ThreadId,
        id: Uuid,
        epoch: u64,
        outcome: SubmissionOutcome,
    ) -> Result<()> {
        if epoch != self.server_operations.epoch {
            return Ok(());
        }
        if self.reconnect.offline {
            self.handle_submission_event(SubmissionEvent::Retire).await;
            return Ok(());
        }
        let Some(key) = self
            .server_operations
            .lanes
            .iter()
            .find_map(|(key, lane)| (lane.active.id == id).then_some(*key))
        else {
            return Ok(());
        };
        let entry = match self.server_operations.lanes.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => entry,
            std::collections::hash_map::Entry::Vacant(_) => {
                unreachable!("operation lane disappeared")
            }
        };
        let SubmissionLane {
            active,
            state,
            mut waiting,
        } = entry.remove();
        drop(state);
        if matches!(&active.request, SubmissionRequest::Continuation { .. })
            && let SubmissionOutcome::Finished {
                result: Err(failure),
                ..
            } = &outcome
        {
            let uncertain = !matches!(failure, SubmissionFailure::Rejected(_));
            self.chat_widget.add_error_message(if uncertain {
                "Continuation outcome is unknown. The original may still be delivered; no retry was sent."
            } else {
                "Couldn’t continue this chat. Review its latest status before trying again."
            }.into());
            let drafts = recoverable_drafts(waiting.drain(..));
            self.restore_submission_drafts(
                thread_id,
                drafts,
                DraftDisposition::Recover { uncertain_id: None },
            )
            .await;
            if uncertain {
                self.server_operations.lanes.insert(
                    key,
                    SubmissionLane {
                        active,
                        state: LaneState::Uncertain,
                        waiting,
                    },
                );
            }
            self.drive_server_operations(server);
            self.refresh_submission_preview();
            return Ok(());
        }
        if let SubmissionOutcome::Operation(result) = outcome {
            let SubmissionRequest::Operation(operation) = &active.request else {
                unreachable!("RPC receipt for a turn operation");
            };
            let applicable = self
                .thread_event_channels
                .get(&thread_id)
                .is_some_and(|channel| Arc::ptr_eq(&channel.store, &active.store));
            if matches!(&result, Ok(OperationReceipt::SettingsUnsupported)) {
                server.note_operation_settings_unsupported();
            }
            match result {
                Ok(receipt) if applicable => {
                    self.finish_server_operation(thread_id, operation, receipt)
                        .await?;
                }
                Ok(_) => {}
                Err(failure) => {
                    self.reject_server_operation(thread_id, operation, &failure);
                    if !matches!(failure, SubmissionFailure::Rejected(_))
                        && !matches!(&**operation, ThreadOperation::Catalog(_))
                    {
                        let drafts = recoverable_drafts(waiting.drain(..));
                        self.restore_submission_drafts(
                            thread_id,
                            drafts,
                            DraftDisposition::Recover { uncertain_id: None },
                        )
                        .await;
                        self.server_operations.lanes.insert(
                            key,
                            SubmissionLane {
                                active,
                                state: LaneState::Uncertain,
                                waiting,
                            },
                        );
                        self.refresh_submission_preview();
                        return Ok(());
                    }
                }
            }
            if let Some(next) = waiting.pop_front() {
                self.server_operations.lanes.insert(
                    key,
                    SubmissionLane {
                        active: next,
                        state: LaneState::Queued,
                        waiting,
                    },
                );
            }
            self.drive_server_operations(server);
            self.refresh_submission_preview();
            return Ok(());
        }
        let current = self.chat_widget.thread_id() == Some(thread_id);
        let mut transport_failed = false;
        let uncertain_id = match &outcome {
            SubmissionOutcome::Finished {
                result: Err(SubmissionFailure::Uncertain(_) | SubmissionFailure::TimedOut),
                ..
            } => active.client_id().map(str::to_owned),
            _ => None,
        };
        let failure = match outcome {
            SubmissionOutcome::Operation(_) => unreachable!("operation receipt handled above"),
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
                    Ok(SubmissionReceipt::Continued(turn)) => {
                        self.finish_misalignment_continuation(thread_id, &active, turn).await;
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
                crate::startup_recovery::acknowledged(
                    active.client_id().expect("user turn has a client identity"),
                );
            }
            if let Some(next) = waiting.pop_front() {
                self.server_operations.lanes.insert(
                    key,
                    SubmissionLane {
                        active: next,
                        state: LaneState::Queued,
                        waiting,
                    },
                );
            }
        }
        self.drive_server_operations(server);
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
            while !self.server_operations.is_empty() {
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
