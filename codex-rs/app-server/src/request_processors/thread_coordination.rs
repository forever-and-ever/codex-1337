//! Typed, frontend-independent access to the authoritative thread services.

use std::sync::Arc;

use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::SandboxMode;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadArchivedNotification;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadItemsListParams;
use codex_app_server_protocol::ThreadItemsListResponse;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadListResponse;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadSetNameParams;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::ThreadTurnsListParams;
use codex_app_server_protocol::ThreadTurnsListResponse;
use codex_app_server_protocol::ThreadUnarchiveParams;
use codex_app_server_protocol::TurnToolOutput;
use codex_core::CodexThread;
use codex_core::CodexThreadSettingsOverrides;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_protocol::ThreadId;
use codex_protocol::mcp::ClientMcpExtensions;

use super::super::thread_input::ensure_direct_input_allowed;
use super::ThreadRequestProcessor;
use super::ThreadResumeTarget;
use super::internal_error;
use super::invalid_request;
use crate::thread_state::ThreadRuntimeLease;

/// A process-owned service. Calling identity is supplied by the extension host,
/// never by model arguments or a frontend connection.
pub(crate) struct ThreadCoordinationService {
    threads: ThreadRequestProcessor,
}

impl ThreadCoordinationService {
    pub(crate) fn new(threads: ThreadRequestProcessor) -> Self {
        Self { threads }
    }

    pub(crate) async fn list(
        &self,
        params: ThreadListParams,
    ) -> Result<ThreadListResponse, JSONRPCErrorError> {
        self.threads.thread_list_response_inner(params).await
    }

    pub(crate) async fn read(&self, thread_id: &str) -> Result<Thread, JSONRPCErrorError> {
        Ok(self
            .threads
            .thread_read_response_inner(ThreadReadParams {
                thread_id: thread_id.to_owned(),
                include_turns: false,
            })
            .await?
            .thread)
    }

    pub(crate) async fn turns(
        &self,
        params: ThreadTurnsListParams,
    ) -> Result<ThreadTurnsListResponse, JSONRPCErrorError> {
        self.threads.thread_turns_list_response_inner(params).await
    }

    pub(crate) async fn items(
        &self,
        params: ThreadItemsListParams,
    ) -> Result<ThreadItemsListResponse, JSONRPCErrorError> {
        self.threads.thread_items_list_response_inner(params).await
    }

    /// Admit messages to existing destinations before a resume, runtime lease,
    /// or turn injection can manufacture activity. Initial task creation does
    /// not use this fence; subscriber age and unattended active turns are valid.
    pub(crate) async fn ensure_message_recipient(
        &self,
        thread_id: &str,
    ) -> Result<(), JSONRPCErrorError> {
        let id = ThreadId::from_string(thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        if self.threads.thread_state_manager.has_subscribers(id).await {
            return Ok(());
        }
        // Metadata-only authoritative read does not restore an unloaded runtime.
        let destination = self.read(thread_id).await?;
        let cutoff = chrono::Utc::now().timestamp().saturating_sub(24 * 60 * 60);
        if matches!(destination.status, ThreadStatus::Active { .. })
            || destination.updated_at >= cutoff
        {
            return Ok(());
        }
        Err(invalid_request(
            "Message undeliverable: destination inactive for more than 24 hours, with no connected subscriber or active turn. Open it or send it a user message to re-enable delivery.",
        ))
    }

    pub(crate) async fn load(
        &self,
        thread_id: &str,
    ) -> Result<(Arc<CodexThread>, ThreadRuntimeLease), JSONRPCErrorError> {
        let id = ThreadId::from_string(thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        // Do not allocate lifecycle state for arbitrary model-supplied UUIDs.
        self.read(thread_id).await?;
        let lease = self.threads.thread_state_manager.lease_runtime(id).await;
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(/*secs*/ 10);
        while self
            .threads
            .pending_thread_unloads
            .lock()
            .await
            .contains(&id)
        {
            if tokio::time::Instant::now() >= deadline {
                return Err(invalid_request("destination runtime is still closing"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(/*millis*/ 20)).await;
        }
        self.threads
            .thread_resume(
                ThreadResumeTarget::Coordination,
                ThreadResumeParams {
                    thread_id: thread_id.to_owned(),
                    exclude_turns: true,
                    ..Default::default()
                },
                /*app_server_client_name*/ None,
                /*app_server_client_version*/ None,
                ClientMcpExtensions::default(),
            )
            .await?;
        self.threads
            .thread_manager
            .get_thread(id)
            .await
            .map(|thread| (thread, lease))
            .map_err(|err| internal_error(format!("failed to load thread: {err}")))
    }

    pub(crate) async fn send(
        &self,
        thread_id: &str,
        output: TurnToolOutput,
        model: Option<String>,
    ) -> Result<(), JSONRPCErrorError> {
        let (thread, _lease) = self.load(thread_id).await?;
        // Server ownership does not grant direct input to parent-owned V2 agents.
        ensure_direct_input_allowed(thread.as_ref()).await?;
        self.threads
            .config_manager
            .check_thread_model_provider(thread.config().await.as_ref())
            .await
            .map_err(|err| super::config_load_error(&err))?;
        let submission = thread
            .start_or_steer_turn(
                TurnInputRequest::new(TurnInput::ResponseItem(output.into_core()))
                    .with_thread_settings(CodexThreadSettingsOverrides {
                        model,
                        ..Default::default()
                    }),
            )
            .await
            .map_err(|err| internal_error(format!("failed to deliver message: {err}")))?;
        match submission {
            TurnInputSubmission::Started { .. } | TurnInputSubmission::Steered { .. } => Ok(()),
            TurnInputSubmission::NotSubmitted { reason } => Err(invalid_request(format!(
                "message was not delivered: {reason:?}"
            ))),
        }
    }

    pub(crate) async fn create(
        &self,
        caller: ThreadId,
        model: Option<String>,
    ) -> Result<(ThreadStartResponse, ThreadRuntimeLease), JSONRPCErrorError> {
        let source = self
            .threads
            .thread_manager
            .get_thread(caller)
            .await
            .map_err(|err| invalid_request(format!("calling thread is unavailable: {err}")))?;
        let snapshot = source.config_snapshot().await;
        if snapshot.ephemeral {
            return Err(invalid_request(
                "ephemeral tasks cannot create inspectable background tasks",
            ));
        }
        let source_thread = self.read(&caller.to_string()).await?;
        // Independent tasks keep thread/start account semantics: resolve the
        // chosen provider's current/default account, not the caller's family
        // identity. Existing destinations instead resume their saved identity.
        let mut params = ThreadStartParams {
            model: Some(model.unwrap_or_else(|| snapshot.model.clone())),
            model_provider: Some(snapshot.model_provider_id.clone()),
            service_tier: Some(snapshot.service_tier.clone()),
            cwd: Some(snapshot.cwd().to_string_lossy().into_owned()),
            runtime_workspace_roots: Some(snapshot.workspace_roots.clone()),
            approval_policy: Some(snapshot.approval_policy.into()),
            approvals_reviewer: Some(snapshot.approvals_reviewer.into()),
            history_mode: Some(snapshot.history_mode.into()),
            project_id: source_thread.project_id,
            ..Default::default()
        };
        if let Some(profile) = &snapshot.active_permission_profile {
            params.permissions = Some(profile.id.clone());
        } else {
            params.sandbox = Some(match snapshot.sandbox_policy() {
                codex_protocol::protocol::SandboxPolicy::DangerFullAccess => {
                    SandboxMode::DangerFullAccess
                }
                codex_protocol::protocol::SandboxPolicy::ReadOnly { .. } => SandboxMode::ReadOnly,
                codex_protocol::protocol::SandboxPolicy::WorkspaceWrite { .. } => {
                    SandboxMode::WorkspaceWrite
                }
                codex_protocol::protocol::SandboxPolicy::ExternalSandbox { .. } => {
                    return Err(invalid_request(
                        "Cannot inherit an external sandbox without a permission profile",
                    ));
                }
            });
        }
        // Inherit sampling settings, not whichever renderer happens to be attached.
        if let Some(effort) = snapshot.reasoning_effort {
            params.config.get_or_insert_default().insert(
                "model_reasoning_effort".into(),
                serde_json::to_value(effort).map_err(|err| internal_error(err.to_string()))?,
            );
        }
        let (response, lease) = self
            .threads
            .thread_start_inner(
                /*request_id*/ None,
                params,
                /*app_server_client_name*/ None,
                /*app_server_client_version*/ None,
                source.client_mcp_extensions(),
                /*request_context*/ None,
            )
            .await?;
        Ok((
            response,
            lease.expect("headless startup retains its runtime lease"),
        ))
    }

    pub(crate) async fn fork(
        &self,
        params: ThreadForkParams,
    ) -> Result<ThreadForkResponse, JSONRPCErrorError> {
        Box::pin(self.threads.thread_fork_inner(
            /*request_id*/ None,
            params,
            /*app_server_client_name*/ None,
            /*app_server_client_version*/ None,
            ClientMcpExtensions::default(),
        ))
        .await
    }

    pub(crate) async fn rename(
        &self,
        params: ThreadSetNameParams,
    ) -> Result<(), JSONRPCErrorError> {
        let (_, notification) = self.threads.thread_set_name_response_inner(params).await?;
        if let Some(notification) = notification {
            self.threads
                .outgoing
                .send_server_notification(ServerNotification::ThreadNameUpdated(notification))
                .await;
        }
        Ok(())
    }

    pub(crate) async fn archive(
        &self,
        params: ThreadArchiveParams,
    ) -> Result<(), JSONRPCErrorError> {
        let (_, thread_ids) = self.threads.thread_archive_inner(params).await?;
        for thread_id in thread_ids {
            self.threads
                .outgoing
                .send_server_notification(ServerNotification::ThreadArchived(
                    ThreadArchivedNotification { thread_id },
                ))
                .await;
        }
        Ok(())
    }

    pub(crate) async fn unarchive(
        &self,
        params: ThreadUnarchiveParams,
    ) -> Result<(), JSONRPCErrorError> {
        let (_, notification) = self.threads.thread_unarchive_inner(params).await?;
        self.threads
            .outgoing
            .send_server_notification(ServerNotification::ThreadUnarchived(notification))
            .await;
        Ok(())
    }
}
