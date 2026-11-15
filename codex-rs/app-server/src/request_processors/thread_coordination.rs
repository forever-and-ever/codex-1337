//! Typed, frontend-independent access to the authoritative thread services.

use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::turn_input::TurnStartOptions;

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
use codex_app_server_protocol::ThreadUnarchivedNotification;
use codex_app_server_protocol::TurnToolOutput;
use codex_core::CodexThread;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_protocol::ThreadId;
use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::ThreadSettingsOverrides;
use serde_json::json;

use super::super::thread_input::ensure_direct_input_allowed;
use super::ServerResume;
use super::ThreadRequestProcessor;
use super::ThreadResumeTarget;
use super::internal_error;
use super::invalid_request;
use super::thread_fork::ThreadForkOrigin;
use super::thread_start::HeadlessStart;
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

    pub(crate) async fn resolve_message_name(
        &self,
        name: &str,
    ) -> Result<(String, String), JSONRPCErrorError> {
        const MAX_CANDIDATES: usize = 50;
        let name = name.trim();
        if name.is_empty() {
            return Err(invalid_request("threadName must not be empty"));
        }
        let _permit = self.threads.acquire_thread_list_state_permit().await?;
        let state = self.threads.state_db.as_ref().ok_or_else(|| {
            internal_error("threadName resolution is unavailable without authoritative state")
        })?;
        self.threads
            .thread_store
            .ensure_thread_name_claims()
            .await
            .map_err(|err| {
                internal_error(format!("failed to load canonical thread names: {err}"))
            })?;
        let owners = state
            .find_thread_name_owners(name)
            .await
            .map_err(|err| internal_error(format!("failed to resolve threadName: {err}")))?;
        match owners.as_slice() {
            [] => {
                let mut error = invalid_request("No unarchived Codex task has this threadName");
                error.data = Some(json!({"type": "threadNameNotFound"}));
                Err(error)
            }
            [owner] => Ok((
                owner.id.to_string(),
                owner
                    .name
                    .clone()
                    .ok_or_else(|| internal_error("threadName owner has no canonical name"))?,
            )),
            _ => {
                let mut error =
                    invalid_request("threadName is ambiguous; use a candidate threadId instead");
                // Historical collisions can exceed the model output budget.
                error.data = Some(json!({
                    "type": "threadNameAmbiguous",
                    "candidateThreadIds": owners.iter().take(MAX_CANDIDATES)
                        .map(|owner| owner.id.to_string()).collect::<Vec<_>>(),
                    "candidateCount": owners.len(),
                    "candidatesTruncated": owners.len() > MAX_CANDIDATES
                }));
                Err(error)
            }
        }
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
        // Metadata-only authoritative read never restores an unloaded runtime.
        let destination = self.read(thread_id).await?;
        super::super::comms_gate::ensure_recipient(&destination)?;
        if self.threads.thread_state_manager.has_subscribers(id).await {
            return Ok(());
        }

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
        if self.threads.thread_manager.get_thread(id).await.is_err() {
            self.read(thread_id).await?;
        }
        let lease = self.threads.thread_state_manager.lease_runtime(id).await;
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(/*secs*/ 10);
        loop {
            let completion = {
                let pending = self.threads.pending_thread_unloads.lock().await;
                pending.get(&id).map(tokio::sync::watch::Sender::subscribe)
            };
            let Some(mut completion) = completion else {
                break;
            };
            if tokio::time::timeout_at(deadline, completion.changed())
                .await
                .is_err()
            {
                return Err(invalid_request("destination runtime is still closing"));
            }
        }
        // A live newly-created thread need not have a materialized rollout.
        // Recheck only after acquiring the lease and draining pending unloads;
        // shutdown runtimes still require authoritative cold restoration below.
        if let Ok(thread) = self.threads.thread_manager.get_thread(id).await {
            if thread.agent_status().await != AgentStatus::Shutdown && lease.bind(&thread) {
                return Ok((thread, lease));
            }
            tokio::time::timeout_at(deadline, thread.wait_for_idle_close())
                .await
                .map_err(|_| invalid_request("destination runtime is still closing"))?;
            if let Ok(current) = self.threads.thread_manager.get_thread(id).await
                && current.agent_status().await != AgentStatus::Shutdown
            {
                if !Arc::ptr_eq(&current, &thread) && lease.bind(&current) {
                    return Ok((current, lease));
                }
                return Err(invalid_request("destination runtime is still closing"));
            }
            // Explicit shutdown can leave its stopped generation registered.
            // Restore that destination through the existing cold-resume path;
            // never return its closed Arc or reopen its admission.
        }
        self.read(thread_id).await?;
        Box::pin(self.threads.thread_resume(
            ThreadResumeTarget::Server(ServerResume::Coordination),
            ThreadResumeParams {
                thread_id: thread_id.to_owned(),
                exclude_turns: true,
                ..Default::default()
            },
            /*app_server_client_name*/ None,
            /*app_server_client_version*/ None,
            ClientMcpExtensions::default(),
        ))
        .await?;
        self.threads
            .thread_manager
            .get_thread(id)
            .await
            .map_err(|err| internal_error(format!("failed to load thread: {err}")))
            .and_then(|thread| {
                if lease.bind(&thread) {
                    Ok((thread, lease))
                } else {
                    Err(invalid_request("destination runtime is closing"))
                }
            })
    }

    pub(crate) async fn send(
        &self,
        thread_id: &str,
        output: TurnToolOutput,
        model: Option<String>,
    ) -> Result<(), JSONRPCErrorError> {
        let (thread, _lease) = Box::pin(self.load(thread_id)).await?;
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
                    .with_thread_settings(ThreadSettingsOverrides {
                        model,
                        ..Default::default()
                    }),
            )
            .await
            .map_err(|err| internal_error(format!("failed to deliver message: {err}")))?;
        match submission {
            TurnInputSubmission::Accepted
            | TurnInputSubmission::Started { .. }
            | TurnInputSubmission::Steered { .. } => Ok(()),
            TurnInputSubmission::NotSubmitted { reason } => Err(invalid_request(format!(
                "message was not delivered: {reason:?}"
            ))),
        }
    }

    pub(crate) async fn deliver_alarm(
        &self,
        thread: &Arc<CodexThread>,
        body: String,
    ) -> Result<TurnInputSubmission, String> {
        ensure_direct_input_allowed(thread)
            .await
            .map_err(|err| err.message)?;
        self.threads
            .config_manager
            .check_thread_model_provider(thread.config().await.as_ref())
            .await
            .map_err(|err| err.to_string())?;
        let sticky = thread
            .thread_extension_data()
            .get::<TurnStartOptions>()
            .map(|options| options.as_ref().clone())
            .unwrap_or_default();
        let item = TurnToolOutput {
            name: "set".into(),
            retain: false,
            namespace: Some("alarm".into()),
            output: FunctionCallOutputBody::Text(body),
        }
        .into_core();
        thread
            .start_or_steer_turn(
                TurnInputRequest::new(TurnInput::ResponseItem(item)).on_start(TurnStartOptions {
                    turn_trigger: Some("alarm".into()),
                    ..sticky
                }),
            )
            .await
            .map_err(|err| err.to_string())
    }

    pub(crate) async fn create(
        &self,
        caller: ThreadId,
        model: Option<String>,
        title: Option<String>,
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
        let _permit = self.threads.acquire_thread_list_state_permit().await?;
        let name = title
            .map(|name| {
                codex_core::util::normalize_thread_name(&name)
                    .ok_or_else(|| invalid_request("title must not be empty"))
            })
            .transpose()?;
        if let Some(name) = &name {
            self.threads
                .release_confirmed_name_owners(
                    name, /*claimant*/ None, /*confirmed_ids*/ None,
                )
                .await?;
        }
        let (mut response, lease) = self
            .threads
            .thread_start_inner(
                HeadlessStart,
                params,
                /*app_server_client_name*/ None,
                /*app_server_client_version*/ None,
                source.client_mcp_extensions(),
            )
            .await?;
        if let Some(name) = name {
            let id = ThreadId::from_string(&response.thread.id)
                .map_err(|err| internal_error(format!("invalid created thread id: {err}")))?;
            response.thread.name = Some(
                self.threads
                    .claim_thread_name(
                        id,
                        &name,
                        codex_app_server_protocol::ThreadNameMode::Explicit,
                    )
                    .await
                    .map_err(|mut err| {
                        let data = err.data.get_or_insert_with(|| json!({}));
                        data["createdThreadId"] = json!(id.to_string());
                        err.message =
                            format!("Task {id} was created, but naming failed: {}", err.message);
                        err
                    })?,
            );
        }
        Ok((response, lease))
    }

    pub(crate) async fn fork(
        &self,
        params: ThreadForkParams,
    ) -> Result<ThreadForkResponse, JSONRPCErrorError> {
        Box::pin(self.threads.thread_fork_inner(
            ThreadForkOrigin::Coordination,
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
        let _thread_list_state_permit = self.threads.acquire_thread_list_state_permit().await?;
        let (_, thread_ids) = self.threads.thread_archive_response(params).await?;
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
        let _thread_list_state_permit = self.threads.acquire_thread_list_state_permit().await?;
        let (_, thread_ids) = self.threads.thread_unarchive_response(params).await?;
        for thread_id in thread_ids {
            self.threads
                .outgoing
                .send_server_notification(ServerNotification::ThreadUnarchived(
                    ThreadUnarchivedNotification {
                        thread_id: thread_id.to_string(),
                    },
                ))
                .await;
        }
        Ok(())
    }
}
