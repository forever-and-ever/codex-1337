//! Typed requests and receipts for the non-turn branches of the ordered operation ledger.

use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::Arc;
use super::ClientRequest;
use super::ConfigBatchWriteParams;
use super::ConfigWriteResponse;
use super::ControlOperation;
use super::ExitMode;
use super::HashSet;
use super::OperationLane;
use super::OperationOrigin;
use super::OperationScope;
use super::PathBuf;
use super::PendingSubmission;
use super::RealtimeOperation;
use super::Result;
use super::SkillsListParams;
use super::SkillsListResponse;
use super::SubmissionFailure;
use super::SubmissionRequest;
use super::ThreadId;
use super::ThreadSettingsUpdateParams;
use super::TypedRequestError;
use super::UiReply;
use super::Uuid;
use super::WrapErr;
use super::WriteStatus;
use super::control;
use super::detach;
use super::navigation;
use super::realtime;
use super::removal;
use super::thread_name;
use super::ui_requests;
use super::voice_preferences;
use crate::app_server_session::typed_requests::SessionRpc;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

#[derive(Clone)]
pub(in crate::app) enum ServerOperation {
    Mutation(ThreadMutation),
    Control(ControlOperation),
    Catalog(CatalogRequest),
    /// TELOS(warpaint): silent hydration of persistent presentation metadata.
    ThreadMetadataRead(ui_requests::UiRequest),
    Connection(ConnectionMutation),
    Realtime(RealtimeOperation),
    Response(Box<ServerRequestDelivery>),
}

#[derive(Clone)]
pub(in crate::app) enum ThreadMutation {
    Compact,
    Rename(
        api::ThreadSetNameParams,
        crate::app_server_session::ManualThreadName,
    ),
    Review(api::ReviewTarget),
    CleanBackgroundTerminals,
    DisarmAlarm(String),
    ServerShell(String),
    Settings(Box<ThreadSettingsUpdateParams>),
    GuardianApproval(serde_json::Value),
    UiRequest(ui_requests::UiRequest),
}

#[derive(Clone)]
pub(in crate::app) enum CatalogRequest {
    Skills {
        params: SkillsListParams,
        cwd: PathBuf,
    },
    UiRequest(ui_requests::UiRequest),
}

#[derive(Clone)]
pub(in crate::app) enum ConnectionMutation {
    ReloadUserConfig,
    Voice(voice_preferences::VoicePreferenceOperation),
    ModelDefaults {
        edits: Vec<api::ConfigEdit>,
        setting: String,
    },
    Removal(removal::RemovalOperation),
    Trust(navigation::trust::TrustOperation),
    UiRequest(ui_requests::UiRequest),
}

#[derive(Clone)]
pub(in crate::app) enum ServerRequestDelivery {
    Resolved {
        reply: codex_app_server_client::AppServerReplyHandle,
        result: serde_json::Value,
    },
    Rejected {
        reply: codex_app_server_client::AppServerReplyHandle,
        error: codex_app_server_protocol::JSONRPCErrorError,
    },
}

impl ServerRequestDelivery {
    async fn send(self) -> std::io::Result<()> {
        match self {
            Self::Resolved { reply, result } => reply.resolve(result).await,
            Self::Rejected { reply, error } => reply.reject(error).await,
        }
    }
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
    Skills(SkillsListResponse),
    ConfigReloaded,
    VoicePreference(voice_preferences::VoicePreferenceReceipt),
    RealtimePrepared(Result<Option<codex_protocol::protocol::RealtimeVoice>, String>),
    RealtimeStarted,
    RealtimeSpeechAccepted,
    RealtimeDiscarded,
    RealtimeStopped,
    ServerRequestDelivered,
    ModelDefaultsSaved(ConfigWriteResponse),
    Removal(removal::RemovalReceipt),
    TrustSaved,
    UiRequest(UiReply),
    UiRequestApplied,
}

impl ServerOperation {
    pub(in crate::app) fn label(&self) -> &'static str {
        match self {
            Self::Response(_) => "Delivering app-server response",
            Self::Realtime(operation) => operation.label(),
            Self::Control(ControlOperation::RealtimeStop { .. }) => "Stopping voice mode",
            Self::Control(ControlOperation::DetachAndExit { .. }) => {
                "Detaching frontend before exit"
            }
            Self::Control(ControlOperation::Interrupt { .. }) => "Interrupting turn",
            Self::Control(ControlOperation::InterruptActive { .. }) => "Stopping background task",
            Self::Control(ControlOperation::StopAndExit { .. }) => "Stopping task before exit",
            Self::Mutation(mutation) => match mutation {
                ThreadMutation::Compact => "Starting compaction",
                ThreadMutation::Rename(..) => "Renaming conversation",
                ThreadMutation::Review(_) => "Starting review",
                ThreadMutation::CleanBackgroundTerminals => "Cleaning background terminals",
                ThreadMutation::DisarmAlarm(_) => "Disarming alarm",
                ThreadMutation::ServerShell(_) => "Running shell command on server",
                ThreadMutation::Settings(_) => "Updating conversation settings",
                ThreadMutation::GuardianApproval(_) => "Approving reviewed action",
                ThreadMutation::UiRequest(request) => request.label(),
            },
            Self::Catalog(CatalogRequest::Skills { .. }) => "Loading skills",
            Self::Catalog(CatalogRequest::UiRequest(request))
            | Self::ThreadMetadataRead(request)
            | Self::Connection(ConnectionMutation::UiRequest(request)) => request.label(),
            Self::Connection(ConnectionMutation::Trust(operation)) => operation.label(),
            Self::Connection(ConnectionMutation::ReloadUserConfig) => "Reloading settings",
            Self::Connection(ConnectionMutation::Removal(operation)) => operation.label(),
            Self::Connection(ConnectionMutation::ModelDefaults { .. }) => "Saving default model",
            Self::Connection(ConnectionMutation::Voice(operation)) => match operation {
                voice_preferences::VoicePreferenceOperation::Open { .. } => {
                    "Loading voice preferences"
                }
                voice_preferences::VoicePreferenceOperation::Save { .. } => {
                    "Saving voice preference"
                }
                voice_preferences::VoicePreferenceOperation::Confirm { .. } => {
                    "Confirming saved voice preference"
                }
            },
        }
    }

    pub(super) fn ui_request(&self) -> Option<&ui_requests::UiRequest> {
        match self {
            Self::Mutation(ThreadMutation::UiRequest(request))
            | Self::Catalog(CatalogRequest::UiRequest(request))
            | Self::ThreadMetadataRead(request)
            | Self::Connection(ConnectionMutation::UiRequest(request)) => Some(request),
            _ => None,
        }
    }

    pub(super) fn cancellable_before_send(&self) -> bool {
        if let Self::Connection(ConnectionMutation::Removal(operation)) = self {
            return operation.cancellable_before_send();
        }
        !matches!(
            self,
            Self::Control(_)
                | Self::Response(_)
                | Self::Connection(ConnectionMutation::Voice(
                    voice_preferences::VoicePreferenceOperation::Confirm { .. }
                ))
        )
    }

    pub(super) fn lane(&self, scope: OperationScope) -> OperationLane {
        let thread_id = || match scope {
            OperationScope::Thread(id) => id,
            OperationScope::Connection => {
                unreachable!("thread-only operations are admitted with a thread origin")
            }
        };
        match self {
            Self::Response(_) => OperationLane::Response(scope),
            Self::Realtime(_) => OperationLane::Realtime(thread_id()),
            Self::Connection(_) => OperationLane::Connection,
            Self::Control(ControlOperation::DetachAndExit { .. }) => OperationLane::Exit,
            Self::Control(_) => OperationLane::Control(thread_id()),
            Self::ThreadMetadataRead(_) => OperationLane::ThreadMetadata(thread_id()),
            Self::Mutation(_) | Self::Catalog(_) => OperationLane::Thread(thread_id()),
        }
    }
}

pub(super) async fn send(
    handle: AppServerRequestHandle,
    id: Uuid,
    operation: ServerOperation,
    origin: OperationOrigin,
) -> Result<OperationReceipt, TypedRequestError> {
    if let Some(request) = operation.ui_request() {
        return request.send(handle).await.map(OperationReceipt::UiRequest);
    }
    let request_id = api::RequestId::String(format!("tui-operation-{id}"));
    match operation {
        ServerOperation::Response(delivery) => {
            delivery
                .send()
                .await
                .map_err(|source| TypedRequestError::Transport {
                    method: "server-request response delivery".into(),
                    source,
                })?;
            return Ok(OperationReceipt::ServerRequestDelivered);
        }
        ServerOperation::Connection(ConnectionMutation::Trust(operation)) => {
            return operation.send(handle, id).await;
        }
        ServerOperation::Connection(ConnectionMutation::ReloadUserConfig) => {
            let _: ConfigWriteResponse = handle
                .request_typed(ClientRequest::ConfigBatchWrite {
                    request_id,
                    params: ConfigBatchWriteParams {
                        edits: Vec::new(),
                        file_path: None,
                        expected_version: None,
                        reload_user_config: true,
                    },
                })
                .await?;
            return Ok(OperationReceipt::ConfigReloaded);
        }
        ServerOperation::Connection(ConnectionMutation::ModelDefaults { edits, .. }) => {
            let response = handle
                .request_typed(ClientRequest::ConfigBatchWrite {
                    request_id,
                    params: ConfigBatchWriteParams {
                        edits,
                        file_path: None,
                        expected_version: None,
                        reload_user_config: true,
                    },
                })
                .await?;
            return Ok(OperationReceipt::ModelDefaultsSaved(response));
        }
        ServerOperation::Connection(ConnectionMutation::Removal(operation)) => {
            return removal::send(handle, id, operation).await;
        }
        ServerOperation::Connection(ConnectionMutation::Voice(operation)) => {
            return voice_preferences::send(handle, id, operation).await;
        }
        _ => {}
    }
    let (thread_id, store) = origin.thread();
    let thread = thread_id.to_string();
    match operation {
        ServerOperation::Response(_) => unreachable!("response handled before thread projection"),
        ServerOperation::Realtime(operation) => {
            realtime::send(handle, thread_id, id, operation).await
        }
        ServerOperation::Control(operation) => {
            control::send(&handle, thread_id, id, operation, store).await
        }
        ServerOperation::Mutation(mutation) => match mutation {
            ThreadMutation::UiRequest(_) => {
                unreachable!("UI request dispatched before thread projection")
            }
            ThreadMutation::Compact => {
                let _: api::ThreadCompactStartResponse = handle
                    .request_typed(ClientRequest::ThreadCompactStart {
                        request_id,
                        params: api::ThreadCompactStartParams { thread_id: thread },
                    })
                    .await?;
                Ok(OperationReceipt::Compacted)
            }
            ThreadMutation::Rename(params, writer) => {
                let name = params.name.clone();
                let response = writer.save(request_id, params).await?;
                Ok(OperationReceipt::Renamed(
                    response.thread_name.unwrap_or(name),
                ))
            }
            ThreadMutation::Review(target) => {
                let previous_latest = store.lock().await.latest_turn_id.clone();
                let response = handle
                    .request_typed(ClientRequest::ReviewStart {
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
                    .request_typed(ClientRequest::ThreadBackgroundTerminalsClean {
                        request_id,
                        params: api::ThreadBackgroundTerminalsCleanParams { thread_id: thread },
                    })
                    .await?;
                Ok(OperationReceipt::TerminalsCleaned)
            }
            ThreadMutation::DisarmAlarm(id) => {
                let _: api::ThreadAlarmDisarmResponse = handle
                    .request_typed(ClientRequest::ThreadAlarmDisarm {
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
                    .request_typed(ClientRequest::ThreadShellCommand {
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
                match handle.thread_settings_update(request_id, *params).await {
                    Ok(true) => Ok(OperationReceipt::SettingsUpdated),
                    Ok(false) => Ok(OperationReceipt::SettingsUnsupported),
                    Err(error) => Err(error),
                }
            }
            ThreadMutation::GuardianApproval(event) => {
                let _: api::ThreadApproveGuardianDeniedActionResponse = handle
                    .request_typed(ClientRequest::ThreadApproveGuardianDeniedAction {
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
        ServerOperation::Catalog(CatalogRequest::UiRequest(_))
        | ServerOperation::ThreadMetadataRead(_) => {
            unreachable!("UI request dispatched before thread projection")
        }
        ServerOperation::Catalog(CatalogRequest::Skills { params, .. }) => handle
            .request_typed(ClientRequest::SkillsList { request_id, params })
            .await
            .map(OperationReceipt::Skills),
        ServerOperation::Connection(_) => {
            unreachable!("connection request handled before thread projection")
        }
    }
}

impl App {
    pub(in crate::app) fn enqueue_server_operation(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        operation: ServerOperation,
    ) {
        self.enqueue_submission(
            server,
            thread_id,
            SubmissionRequest::Operation(Box::new(operation)),
        );
    }

    pub(super) async fn finish_server_operation(
        &mut self,
        origin: &OperationOrigin,
        operation: &ServerOperation,
        receipt: OperationReceipt,
    ) -> Result<()> {
        if let (
            ServerOperation::Connection(ConnectionMutation::ModelDefaults { setting, .. }),
            OperationReceipt::ModelDefaultsSaved(response),
        ) = (operation, &receipt)
        {
            if response.status == WriteStatus::OkOverridden {
                self.chat_widget.add_warning_message(format!("Saved {setting}, but a higher-priority configuration layer overrides the saved value."));
            }
            return Ok(());
        }
        if matches!(origin, OperationOrigin::Connection) {
            if let ServerOperation::Connection(ConnectionMutation::Voice(operation)) = operation
                && let OperationReceipt::VoicePreference(receipt) = receipt
            {
                self.finish_voice_preference(operation, receipt);
            }
            return Ok(());
        }
        let thread_id = origin.thread().0;
        match receipt {
            OperationReceipt::UiRequest(_) => {
                unreachable!("typed UI reply reduced before the operation lane")
            }

            OperationReceipt::RealtimeSpeechAccepted => {
                if let ServerOperation::Realtime(
                    operation @ RealtimeOperation::Speech { delivery_id, .. },
                ) = operation
                    && self.realtime_operation_is_current(thread_id, operation)
                    && let Some(widget) = self.voice_widget_for_thread(thread_id)
                {
                    widget.accept_realtime_speech(*delivery_id);
                }
            }

            OperationReceipt::StopAndExitCompleted => self
                .app_event_tx
                .send(AppEvent::Exit(ExitMode::ShutdownAfterInterrupt)),

            OperationReceipt::ExitDetached(receipt) => {
                if let ServerOperation::Control(ControlOperation::DetachAndExit { plan, reason }) =
                    operation
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
                if let ServerOperation::Catalog(CatalogRequest::Skills { cwd, .. }) = operation
                    && self.chat_widget.config_ref().cwd.as_path() == cwd
                {
                    self.handle_skills_list_result(Ok(response), "failed to refresh skills");
                }
            }
            OperationReceipt::SettingsUpdated => {
                if let ServerOperation::Mutation(ThreadMutation::Settings(params)) = operation {
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

            OperationReceipt::TrustSaved => unreachable!("trust writes have connection origin"),
            OperationReceipt::ModelDefaultsSaved(_) => {
                unreachable!("model defaults handled before origin projection")
            }
            OperationReceipt::VoicePreference(_) => {
                unreachable!("voice preferences have connection origin")
            }
            OperationReceipt::ServerRequestDelivered
            | OperationReceipt::UiRequestApplied
            | OperationReceipt::Removal(_)
            | OperationReceipt::RealtimePrepared(_)
            | OperationReceipt::RealtimeStarted
            | OperationReceipt::RealtimeDiscarded
            | OperationReceipt::RealtimeStopped
            | OperationReceipt::Interrupted
            | OperationReceipt::SettingsUnsupported
            | OperationReceipt::Compacted
            | OperationReceipt::TerminalsCleaned
            | OperationReceipt::AlarmDisarmed
            | OperationReceipt::ServerShellFinished
            | OperationReceipt::GuardianApproved
            | OperationReceipt::ConfigReloaded => {}
        }
        Ok(())
    }

    pub(super) fn reject_server_operation(
        &mut self,
        origin: &OperationOrigin,
        operation: &ServerOperation,
        failure: &SubmissionFailure,
    ) {
        if let ServerOperation::Connection(ConnectionMutation::Trust(operation)) = operation {
            let scope = operation.scope();
            let message = match failure {
                SubmissionFailure::Rejected(error) => format!(
                    "Trust could not be saved for {scope}: {error}. The navigation was not resumed."
                ),
                SubmissionFailure::Uncertain(error) => format!(
                    "Trust save outcome for {scope} is unknown: {error}. No write was retried. Close and reopen this TUI to reread trust settings; the earlier write may still complete."
                ),
                SubmissionFailure::TimedOut | SubmissionFailure::Cancelled => format!(
                    "Trust save acknowledgement for {scope} timed out. No write was retried. Close and reopen this TUI to reread trust settings; the earlier write may still complete."
                ),
            };
            self.chat_widget.add_error_message(message);
            return;
        }
        if let ServerOperation::Connection(ConnectionMutation::Voice(
            voice_preferences::VoicePreferenceOperation::Confirm { .. },
        )) = operation
        {
            self.chat_widget.add_error_message("Voice preference was saved, but its effective value could not be confirmed. No write was retried.".into());
            return;
        }
        let thread_id = origin.thread_id();
        if let Some(thread_id) = thread_id
            && let ServerOperation::Realtime(operation) = operation
        {
            self.reject_realtime_operation(thread_id, operation, failure);
            return;
        }
        if let Some(thread_id) = thread_id
            && let ServerOperation::Mutation(ThreadMutation::Rename(params, _)) = operation
            && let SubmissionFailure::Rejected(error) = failure
            && self.confirm_thread_name_conflict(
                error,
                thread_name::ThreadNameRequest::Rename {
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
            SubmissionFailure::TimedOut | SubmissionFailure::Cancelled => format!(
                "{} acknowledgement timed out. The server may still complete it; no retry was sent.",
                operation.label()
            ),
        };
        if let Some(thread_id) = thread_id
            && let ServerOperation::Mutation(ThreadMutation::Rename(params, _)) = operation
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
        if matches!(
            operation,
            ServerOperation::Control(ControlOperation::InterruptActive { .. })
        ) {
            self.add_agents_overview_error(message);
            return;
        }
        if let Some(thread_id) = thread_id.filter(|id| self.chat_widget.thread_id() != Some(*id)) {
            self.chat_widget
                .add_error_message(format!("Conversation {thread_id}: {message}"));
        } else {
            self.chat_widget.add_error_message(message);
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
            let OperationOrigin::Thread { thread_id, store } = &entry.origin else {
                continue;
            };
            let thread_id = *thread_id;
            if !self
                .thread_event_channels
                .get(&thread_id)
                .is_some_and(|channel| Arc::ptr_eq(&channel.store, store))
            {
                continue;
            }
            match &**operation {
                ServerOperation::Realtime(operation) => {
                    self.cancel_unsent_realtime_operation(thread_id, operation);
                }
                ServerOperation::Mutation(ThreadMutation::DisarmAlarm(id)) => {
                    if let Some(widget) = self.voice_widget_for_thread(thread_id) {
                        widget.cancel_unsent_alarm_disarm(id);
                    }
                }
                ServerOperation::Mutation(ThreadMutation::Compact | ThreadMutation::Review(_)) => {
                    let outstanding = self.server_operations.lanes.values().any(|lane| {
                        std::iter::once(&lane.active)
                            .chain(lane.waiting.iter())
                            .any(|other| {
                                other.origin.thread_id() == Some(thread_id)
                                    && holds_start_lease(other)
                            })
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
                    if let Some(input) = store.lock().await.input_state.as_mut()
                        && input.input_queue.cancel_unsent_operation()
                    {
                        input.task_running = input.agent_turn_running;
                    }
                }
                // These payloads own no widget-local pending lease before dispatch.
                ServerOperation::Response(_)
                | ServerOperation::Mutation(_)
                | ServerOperation::Catalog(_)
                | ServerOperation::ThreadMetadataRead(_)
                | ServerOperation::Connection(_)
                | ServerOperation::Control(_) => {}
            }
        }
    }
}

pub(super) fn holds_start_lease(entry: &PendingSubmission) -> bool {
    matches!(&entry.request, SubmissionRequest::Operation(operation) if matches!(&**operation,
        ServerOperation::Mutation(ThreadMutation::Compact | ThreadMutation::Review(_))))
}
