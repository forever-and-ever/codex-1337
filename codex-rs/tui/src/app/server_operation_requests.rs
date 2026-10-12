//! Typed requests and receipts for the non-turn branches of the ordered operation ledger.

use super::*;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

#[derive(Clone)]
pub(in crate::app) enum ThreadOperation {
    Mutation(ThreadMutation),
    Control(control::ControlOperation),
    Catalog(CatalogRequest),
    Connection(ConnectionMutation),
    Realtime(realtime::RealtimeOperation),
    Response(Box<ServerRequestDelivery>),
}

#[derive(Clone)]
pub(in crate::app) enum ThreadMutation {
    Compact,
    Rename(api::ThreadSetNameParams),
    Review(api::ReviewTarget),
    CleanBackgroundTerminals,
    DisarmAlarm(String),
    ServerShell(String),
    Settings(api::ThreadSettingsUpdateParams),
    GuardianApproval(serde_json::Value),
}

#[derive(Clone)]
pub(in crate::app) enum CatalogRequest {
    Skills {
        params: api::SkillsListParams,
        cwd: PathBuf,
    },
}

#[derive(Clone)]
pub(in crate::app) enum ConnectionMutation {
    ReloadUserConfig,
}

#[derive(Clone)]
pub(in crate::app) struct ServerRequestDelivery {
    pub(in crate::app) resolution: super::super::app_server_requests::AppServerRequestResolution,
    pub(in crate::app) command: AppCommand,
}

#[derive(Debug)]
pub(in crate::app) enum OperationReceipt {
    Compacted,
    Interrupted,
    ExitDetached(detach::DetachReceipt),
    StopAndExitCompleted,
    Renamed(String),
    Review {
        response: api::ReviewStartResponse,
        previous_latest: Option<String>,
    },
    TerminalsCleaned,
    AlarmDisarmed,
    ServerShellFinished,
    SettingsUpdated,
    SettingsUnsupported,
    GuardianApproved,
    Skills(api::SkillsListResponse),
    ConfigReloaded,
    RealtimePrepared(Result<Option<codex_protocol::protocol::RealtimeVoice>, String>),
    RealtimeStarted,
    RealtimeSpeechAccepted,
    RealtimeDiscarded,
    RealtimeStopped,
    ServerRequestDelivered,
}

impl ThreadOperation {
    pub(in crate::app) fn label(&self) -> &'static str {
        match self {
            Self::Response(_) => "Delivering app-server response",
            Self::Realtime(operation) => operation.label(),
            Self::Control(control::ControlOperation::RealtimeStop { .. }) => "Stopping voice mode",
            Self::Control(control::ControlOperation::DetachAndExit { .. }) => {
                "Detaching frontend before exit"
            }
            Self::Control(control::ControlOperation::Interrupt { .. }) => "Interrupting turn",
            Self::Control(control::ControlOperation::StopAndExit { .. }) => {
                "Stopping task before exit"
            }
            Self::Mutation(mutation) => match mutation {
                ThreadMutation::Compact => "Starting compaction",
                ThreadMutation::Rename(_) => "Renaming conversation",
                ThreadMutation::Review(_) => "Starting review",
                ThreadMutation::CleanBackgroundTerminals => "Cleaning background terminals",
                ThreadMutation::DisarmAlarm(_) => "Disarming alarm",
                ThreadMutation::ServerShell(_) => "Running shell command on server",
                ThreadMutation::Settings(_) => "Updating conversation settings",
                ThreadMutation::GuardianApproval(_) => "Approving reviewed action",
            },
            Self::Catalog(CatalogRequest::Skills { .. }) => "Loading skills",
            Self::Connection(ConnectionMutation::ReloadUserConfig) => "Reloading settings",
        }
    }

    pub(super) fn cancellable_before_send(&self) -> bool {
        !matches!(self, Self::Control(_) | Self::Response(_))
    }

    pub(super) fn lane(&self, thread_id: ThreadId) -> OperationLane {
        match self {
            Self::Response(_) => OperationLane::Response(thread_id),
            Self::Realtime(_) => OperationLane::Realtime(thread_id),
            Self::Connection(_) => OperationLane::Connection,
            Self::Control(control::ControlOperation::DetachAndExit { .. }) => OperationLane::Exit,
            Self::Control(_) => OperationLane::Control(thread_id),
            Self::Mutation(_) | Self::Catalog(_) => OperationLane::Thread(thread_id),
        }
    }
}

pub(super) async fn send(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    id: Uuid,
    operation: ThreadOperation,
    store: Arc<Mutex<ThreadEventStore>>,
) -> Result<OperationReceipt, TypedRequestError> {
    let request_id = api::RequestId::String(format!("tui-operation-{id}"));
    let thread = thread_id.to_string();
    match operation {
        ThreadOperation::Response(delivery) => {
            handle
                .resolve_server_request(delivery.resolution.request_id, delivery.resolution.result)
                .await
                .map_err(|source| TypedRequestError::Transport {
                    method: "server-request response delivery".into(),
                    source,
                })?;
            Ok(OperationReceipt::ServerRequestDelivered)
        }
        ThreadOperation::Realtime(operation) => {
            realtime::send(handle, thread_id, id, operation).await
        }
        ThreadOperation::Control(operation) => {
            control::send(&handle, thread_id, id, operation, &store).await
        }
        ThreadOperation::Mutation(mutation) => match mutation {
            ThreadMutation::Compact => {
                let _: api::ThreadCompactStartResponse = handle
                    .request_typed(api::ClientRequest::ThreadCompactStart {
                        request_id,
                        params: api::ThreadCompactStartParams { thread_id: thread },
                    })
                    .await?;
                Ok(OperationReceipt::Compacted)
            }
            ThreadMutation::Rename(params) => {
                let name = params.name.clone();
                let response: api::ThreadSetNameResponse = handle
                    .request_typed(api::ClientRequest::ThreadSetName { request_id, params })
                    .await?;
                Ok(OperationReceipt::Renamed(
                    response.thread_name.unwrap_or(name),
                ))
            }
            ThreadMutation::Review(target) => {
                let previous_latest = store.lock().await.latest_turn_id.clone();
                let response = handle
                    .request_typed(api::ClientRequest::ReviewStart {
                        request_id,
                        params: api::ReviewStartParams {
                            thread_id: thread,
                            target,
                            delivery: Some(api::ReviewDelivery::Inline),
                        },
                    })
                    .await?;
                Ok(OperationReceipt::Review {
                    response,
                    previous_latest,
                })
            }
            ThreadMutation::CleanBackgroundTerminals => {
                let _: api::ThreadBackgroundTerminalsCleanResponse = handle
                    .request_typed(api::ClientRequest::ThreadBackgroundTerminalsClean {
                        request_id,
                        params: api::ThreadBackgroundTerminalsCleanParams { thread_id: thread },
                    })
                    .await?;
                Ok(OperationReceipt::TerminalsCleaned)
            }
            ThreadMutation::DisarmAlarm(id) => {
                let _: api::ThreadAlarmDisarmResponse = handle
                    .request_typed(api::ClientRequest::ThreadAlarmDisarm {
                        request_id,
                        params: api::ThreadAlarmDisarmParams {
                            thread_id: thread,
                            id,
                        },
                    })
                    .await?;
                Ok(OperationReceipt::AlarmDisarmed)
            }
            ThreadMutation::ServerShell(command) => {
                let _: api::ThreadShellCommandResponse = handle
                    .request_typed(api::ClientRequest::ThreadShellCommand {
                        request_id,
                        params: api::ThreadShellCommandParams {
                            thread_id: thread,
                            command,
                            timeout_ms: None,
                        },
                    })
                    .await?;
                Ok(OperationReceipt::ServerShellFinished)
            }
            ThreadMutation::Settings(params) => {
                let result = handle
                    .request_typed::<api::ThreadSettingsUpdateResponse>(
                        api::ClientRequest::ThreadSettingsUpdate { request_id, params },
                    )
                    .await;
                match result {
                    Ok(_) => Ok(OperationReceipt::SettingsUpdated),
                    Err(TypedRequestError::Server { ref source, .. })
                        if crate::app_server_session::is_thread_settings_update_unsupported(
                            source,
                        ) =>
                    {
                        Ok(OperationReceipt::SettingsUnsupported)
                    }
                    Err(error) => Err(error),
                }
            }
            ThreadMutation::GuardianApproval(event) => {
                let _: api::ThreadApproveGuardianDeniedActionResponse = handle
                    .request_typed(api::ClientRequest::ThreadApproveGuardianDeniedAction {
                        request_id,
                        params: api::ThreadApproveGuardianDeniedActionParams {
                            thread_id: thread,
                            event,
                        },
                    })
                    .await?;
                Ok(OperationReceipt::GuardianApproved)
            }
        },
        ThreadOperation::Catalog(CatalogRequest::Skills { params, .. }) => handle
            .request_typed(api::ClientRequest::SkillsList { request_id, params })
            .await
            .map(OperationReceipt::Skills),
        ThreadOperation::Connection(ConnectionMutation::ReloadUserConfig) => {
            let _: api::ConfigWriteResponse = handle
                .request_typed(api::ClientRequest::ConfigBatchWrite {
                    request_id,
                    params: api::ConfigBatchWriteParams {
                        edits: Vec::new(),
                        file_path: None,
                        expected_version: None,
                        reload_user_config: true,
                    },
                })
                .await?;
            Ok(OperationReceipt::ConfigReloaded)
        }
    }
}

impl App {
    pub(in crate::app) fn enqueue_server_operation(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        operation: ThreadOperation,
    ) {
        self.enqueue_submission(
            server,
            thread_id,
            SubmissionRequest::Operation(Box::new(operation)),
        );
    }

    pub(in crate::app) async fn finish_server_operation(
        &mut self,
        thread_id: ThreadId,
        operation: &ThreadOperation,
        receipt: OperationReceipt,
    ) -> Result<()> {
        match receipt {
            OperationReceipt::ServerRequestDelivered => {
                if let ThreadOperation::Response(delivery) = operation
                    && ThreadEventStore::op_can_change_pending_replay_state(&delivery.command)
                {
                    self.note_thread_outbound_op(thread_id, &delivery.command)
                        .await;
                    self.refresh_pending_thread_approvals().await;
                    self.refresh_side_parent_status_from_store(thread_id).await;
                }
            }
            OperationReceipt::RealtimeSpeechAccepted => {
                if let ThreadOperation::Realtime(
                    operation @ realtime::RealtimeOperation::Speech { delivery_id, .. },
                ) = operation
                    && self.realtime_operation_is_current(thread_id, operation)
                    && let Some(widget) = self.voice_widget_for_thread(thread_id)
                {
                    widget.accept_realtime_speech(*delivery_id);
                }
            }
            OperationReceipt::RealtimePrepared(_)
            | OperationReceipt::RealtimeStarted
            | OperationReceipt::RealtimeDiscarded
            | OperationReceipt::RealtimeStopped => {}
            OperationReceipt::StopAndExitCompleted => self
                .app_event_tx
                .send(AppEvent::Exit(ExitMode::ShutdownAfterInterrupt)),
            OperationReceipt::Interrupted => {}
            OperationReceipt::ExitDetached(receipt) => {
                if let ThreadOperation::Control(control::ControlOperation::DetachAndExit {
                    plan,
                    reason,
                }) = operation
                {
                    self.finish_detach_plan(plan, receipt).await;
                    self.pending_shutdown_exit_thread_id = None;
                    self.app_event_tx
                        .send(AppEvent::ServerOperationExitReady(reason.clone()));
                }
            }
            OperationReceipt::Renamed(name) => {
                self.chat_widget.expect_manual_thread_name(thread_id, name);
                self.cancel_thread_title_generation(thread_id);
            }
            OperationReceipt::Review {
                response,
                previous_latest,
            } => {
                let review_thread_id = ThreadId::from_string(&response.review_thread_id)
                    .wrap_err("review/start returned invalid review thread id")?;
                let store = Arc::clone(&self.ensure_thread_channel(review_thread_id).store);
                let mut store = store.lock().await;
                // A completed turn can precede its acknowledgement.
                if store.latest_turn_id == previous_latest {
                    store.set_active_turn_id(response.turn.id.clone());
                } else if store.latest_turn_id.as_deref() != Some(response.turn.id.as_str()) {
                    return Ok(());
                }
                let completed = store.latest_turn_id.as_deref() == Some(response.turn.id.as_str())
                    && store.active_turn_id().is_none();
                drop(store);
                if !completed && self.chat_widget.thread_id() == Some(thread_id) {
                    self.chat_widget.on_review_started();
                }
            }
            OperationReceipt::Skills(response) => {
                if let ThreadOperation::Catalog(CatalogRequest::Skills { cwd, .. }) = operation
                    && self.chat_widget.config_ref().cwd.as_path() == cwd
                {
                    self.handle_skills_list_result(Ok(response), "failed to refresh skills");
                }
            }
            OperationReceipt::SettingsUpdated => {
                if let ThreadOperation::Mutation(ThreadMutation::Settings(params)) = operation {
                    let permissions_changed = params.cwd.is_some()
                        || params.approval_policy.is_some()
                        || params.approvals_reviewer.is_some()
                        || params.sandbox_policy.is_some()
                        || params.permissions.is_some();
                    if permissions_changed {
                        self.agents_overview
                            .requested_permission_profiles
                            .remove(&thread_id);
                    }
                }
            }
            OperationReceipt::SettingsUnsupported => {}
            OperationReceipt::Compacted
            | OperationReceipt::TerminalsCleaned
            | OperationReceipt::AlarmDisarmed
            | OperationReceipt::ServerShellFinished
            | OperationReceipt::GuardianApproved
            | OperationReceipt::ConfigReloaded => {}
        }
        Ok(())
    }

    pub(in crate::app) fn reject_server_operation(
        &mut self,
        thread_id: ThreadId,
        operation: &ThreadOperation,
        failure: &SubmissionFailure,
    ) {
        if let ThreadOperation::Realtime(operation) = operation {
            self.reject_realtime_operation(thread_id, operation, failure);
            return;
        }
        if let ThreadOperation::Mutation(ThreadMutation::Rename(params)) = operation
            && let SubmissionFailure::Rejected(error) = failure
            && self.confirm_thread_name_conflict(
                error,
                super::super::thread_name::ThreadNameRequest::Rename {
                    thread_id,
                    name: params.name.clone(),
                },
            )
        {
            return;
        }
        let message = match failure {
            SubmissionFailure::Rejected(error) => format!("{} failed: {error}", operation.label()),
            SubmissionFailure::Uncertain(error) => format!(
                "{} outcome is unknown: {error}. No retry was sent.",
                operation.label()
            ),
            SubmissionFailure::TimedOut => format!(
                "{} acknowledgement timed out. The server may still complete it; no retry was sent.",
                operation.label()
            ),
        };
        if let ThreadOperation::Mutation(ThreadMutation::Rename(params)) = operation
            && self.agents_overview.rendered_full_screen
        {
            if let Ok(mut state) = self.agents_overview.view_state.lock() {
                state.set_rename_input(&params.name, &self.keymap);
                state.rename_target = Some(thread_id);
            }
            self.repaint_agents_overview();
            self.add_agents_overview_error(message);
            return;
        }
        if self.chat_widget.thread_id() == Some(thread_id) {
            self.chat_widget.add_error_message(message);
        } else {
            self.chat_widget
                .add_error_message(format!("Conversation {thread_id}: {message}"));
        }
    }
}

impl App {
    pub(super) async fn cleanup_unsent_operations<'a>(
        &mut self,
        entries: impl IntoIterator<Item = &'a PendingSubmission>,
        protected_starts: &HashSet<ThreadId>,
    ) {
        for entry in entries {
            let SubmissionRequest::Operation(operation) = &entry.request else {
                continue;
            };
            let thread_id = entry.thread_id;
            if !self
                .thread_event_channels
                .get(&thread_id)
                .is_some_and(|channel| Arc::ptr_eq(&channel.store, &entry.store))
            {
                continue;
            }
            match &**operation {
                ThreadOperation::Realtime(operation) => {
                    self.cancel_unsent_realtime_operation(thread_id, operation)
                }
                ThreadOperation::Mutation(ThreadMutation::DisarmAlarm(id)) => {
                    if let Some(widget) = self.voice_widget_for_thread(thread_id) {
                        widget.cancel_unsent_alarm_disarm(id);
                    }
                }
                ThreadOperation::Mutation(ThreadMutation::Compact | ThreadMutation::Review(_)) => {
                    let outstanding = self.server_operations.lanes.values().any(|lane| {
                        std::iter::once(&lane.active)
                            .chain(lane.waiting.iter())
                            .any(|other| other.thread_id == thread_id && holds_start_lease(other))
                    });
                    if protected_starts.contains(&thread_id) || outstanding {
                        continue;
                    }
                    if let Some(widget) = self.voice_widget_for_thread(thread_id) {
                        widget.cancel_unsent_turn_operation();
                    }
                    if let Some(input) = self.agents_overview.input_states.get_mut(&thread_id)
                        && input.input_queue.cancel_unsent_operation()
                    {
                        input.task_running = input.agent_turn_running;
                    }
                    if let Some(input) = entry.store.lock().await.input_state.as_mut()
                        && input.input_queue.cancel_unsent_operation()
                    {
                        input.task_running = input.agent_turn_running;
                    }
                }
                // These payloads own no widget-local pending lease before dispatch.
                ThreadOperation::Response(_)
                | ThreadOperation::Mutation(_)
                | ThreadOperation::Catalog(_)
                | ThreadOperation::Connection(_)
                | ThreadOperation::Control(_) => {}
            }
        }
    }
}

pub(super) fn holds_start_lease(entry: &PendingSubmission) -> bool {
    matches!(&entry.request, SubmissionRequest::Operation(operation) if matches!(&**operation,
        ThreadOperation::Mutation(ThreadMutation::Compact | ThreadMutation::Review(_))))
}
