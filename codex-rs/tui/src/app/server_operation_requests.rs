//! Typed requests and receipts for the non-turn branches of the ordered operation ledger.

use super::*;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

#[derive(Clone)]
pub(in crate::app) enum ThreadOperation {
    Mutation(ThreadMutation),
    Catalog(CatalogRequest),
    Connection(ConnectionMutation),
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

#[derive(Debug)]
pub(in crate::app) enum OperationReceipt {
    Compacted,
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
}

impl ThreadOperation {
    pub(in crate::app) fn label(&self) -> &'static str {
        match self {
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

    pub(super) fn lane(&self, thread_id: ThreadId) -> OperationLane {
        match self {
            Self::Connection(_) => OperationLane::Connection,
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
            if matches!(
                operation,
                ThreadOperation::Mutation(ThreadMutation::DisarmAlarm(_))
            ) && matches!(failure, SubmissionFailure::Rejected(_))
            {
                self.chat_widget.alarm_disarm_failed(message);
            } else if matches!(
                operation,
                ThreadOperation::Mutation(ThreadMutation::Compact | ThreadMutation::Review(_))
            ) && matches!(failure, SubmissionFailure::Rejected(_))
            {
                if !self
                    .chat_widget
                    .handle_turn_start_rejection(message.clone())
                {
                    self.chat_widget.add_error_message(message);
                }
            } else {
                self.chat_widget.add_error_message(message);
            }
        } else {
            self.chat_widget
                .add_error_message(format!("Conversation {thread_id}: {message}"));
        }
    }
}
