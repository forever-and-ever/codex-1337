//! Capture a native child without retaining its source runtime.

use super::ForkPersistence;
use super::HistoryInitialization;
use super::PreparedChildOrigin;
use super::ThreadManager;
use super::ThreadReadyNotification;
use super::ThreadSpawnRequest;
use crate::StartCredentials;
use crate::agent::control::AgentControlInit;
use crate::agent::control::PendingSpawn;
use crate::agent::control::SpawnOwner;
use crate::agent::control::effective_spawn_config;
use crate::agent::host::HostThreadSpawnReservation;
use crate::agent::host::PreparedChildInheritance;
use crate::agent::host::ToolInvocationContext;
use crate::agent::host::prepare_spawn_inputs;
use crate::agent::host::with_live_invocation;
use crate::agent::types::AgentMetadata;
use crate::agent::types::LiveAgent;
use crate::codex_thread::CodexThread;
use crate::exec_policy::ExecPolicyManager;
use crate::runtime_auth::RuntimeAuthSlot;
use crate::session::session::AppServerClientMetadata;
use crate::thread_manager::StartThreadOptions;
use crate::thread_spawn::ThreadCreateRequest;
use crate::thread_spawn::ThreadSpawnError;
use crate::thread_spawn::ThreadSpawnResult;
use codex_config::McpServerAuth;
use codex_features::Feature;
use codex_history::InitialHistory;
use codex_login::AuthManager;
use codex_protocol::AgentPath;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::error::AgentErrorContext;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_protocol::protocol::MultiAgentVersion;
use codex_thread_store::ListThreadAttachmentsParams;
use std::sync::Arc;

/// Only live invocation admission can produce this detached authority.
pub(crate) struct PreparedTransportedChild {
    caller: ThreadId,
    session_id: SessionId,
    agent_path: AgentPath,
    options: StartThreadOptions,
    inheritance: PreparedChildInheritance,
    client_metadata: AppServerClientMetadata,
    auth_manager: Arc<AuthManager>,
    runtime_auth: RuntimeAuthSlot,
}

impl PreparedTransportedChild {
    pub(crate) fn agent_path(&self) -> &AgentPath {
        &self.agent_path
    }
}

pub(crate) async fn prepare(
    parent: &CodexThread,
    invocation: &ToolInvocationContext,
    request: ThreadCreateRequest,
) -> Result<PreparedTransportedChild, ThreadSpawnError> {
    Box::pin(with_live_invocation(
        parent,
        invocation,
        |step| async move {
            let (settings, agent_path) = Box::pin(crate::thread_spawn::prepare(
                &parent.session,
                &step,
                &invocation.call_id,
                request,
            ))
            .await?;
            let config = &settings.config;
            if config.application_network_policy.is_managed()
                || config.application_auth_route_config.is_some()
                || config.config_layer_stack.cloud_config_binding().is_some()
            {
                return Err(ThreadSpawnError::Preparation(
                "prepared spawn requires destination binding for host-managed application authority"
                    .into(),
            ));
            }
            if config.features.enabled(Feature::UseXaa)
                && config.mcp_enterprise_managed_auth.is_some()
                && config.mcp_servers.get().values().any(|server| {
                    server.enabled
                        && matches!(server.auth, McpServerAuth::EmaAuth)
                        && server.validate_ema_auth_transport().is_ok()
                })
            {
                return Err(ThreadSpawnError::Preparation(
                    "prepared spawn requires destination binding for EMA-authenticated MCP servers"
                        .into(),
                ));
            }
            let environments = settings
                .options
                .environments
                .as_ref()
                .ok_or_else(|| {
                    ThreadSpawnError::Preparation(
                        "prepared spawn requires captured environments".into(),
                    )
                })?
                .inheritable_requests();
            if environments
                .iter()
                .any(|environment| matches!(environment.config, EnvironmentConfigState::Pending))
            {
                return Err(ThreadSpawnError::Preparation(
                    "prepared spawn requires resolved environment configuration".into(),
                ));
            }
            let store = &parent.session.services.thread_store;
            if store.supports_thread_attachments() {
                let page = store
                    .list_thread_attachments(ListThreadAttachmentsParams {
                        thread_id: parent.session.thread_id,
                        cursor: None,
                        limit: 1,
                    })
                    .await
                    .map_err(|error| {
                        CodexErr::InvalidRequest(format!(
                            "cannot prepare child attachments: {error}"
                        ))
                    })?;
                if !page.attachments.is_empty() {
                    return Err(ThreadSpawnError::Preparation(
                        "prepared spawn requires a host handoff for thread attachments".into(),
                    ));
                }
            }
            let provider = parent
                .capture_provider()
                .await
                .map_err(|error| ThreadSpawnError::Preparation(error.to_string()))?;
            provider
                .matches(&config.model_provider)
                .map_err(|error| ThreadSpawnError::Preparation(error.to_string()))?;
            // Native preparation owns history provenance, checkpoint filtering and cache policy.
            let (mut options, mut inheritance, client_metadata) =
                Box::pin(prepare_spawn_inputs(parent, settings)).await?;
            if inheritance.instructions.thread_provider.is_some()
                || inheritance.instructions.user_provider.is_some()
            {
                return Err(ThreadSpawnError::Preparation(
                "prepared spawn requires destination binding for inherited instruction providers"
                    .into(),
            ));
            }
            options.environments = Some(environments);
            options.inherited_environments = None;
            options.credentials = StartCredentials::Captured(provider);
            inheritance.attachment_source = None;
            let policy = match inheritance.exec_policy.take() {
                Some(policy) => policy,
                None => Arc::new(
                    ExecPolicyManager::load(&options.config.config_layer_stack)
                        .await
                        .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?,
                ),
            };
            // Freeze after history and every other awaited source read.
            let (policy, requirements) = policy.snapshot();
            inheritance.exec_policy = Some(Arc::new(ExecPolicyManager::from_snapshot(
                policy,
                requirements,
            )));
            let _active = invocation.lock_active()?;
            Ok(PreparedTransportedChild {
                caller: parent.session.thread_id,
                session_id: parent.session.session_id(),
                agent_path,
                options,
                inheritance,
                client_metadata,
                auth_manager: Arc::clone(&parent.session.services.auth_manager),
                runtime_auth: Arc::clone(&parent.session.services.runtime_auth),
            })
        },
    ))
    .await
}

pub(crate) async fn start(
    destination: &ThreadManager,
    prepared: PreparedTransportedChild,
    reservation: HostThreadSpawnReservation,
) -> Result<ThreadSpawnResult, ThreadSpawnError> {
    let PreparedTransportedChild {
        caller,
        session_id,
        agent_path,
        mut options,
        inheritance,
        client_metadata,
        auth_manager,
        runtime_auth,
    } = prepared;
    if reservation.control.identity() != session_id
        || reservation.thread_id == caller
        || SessionId::from(reservation.thread_id) == session_id
    {
        return Err(ThreadSpawnError::Preparation(
            "reservation does not match prepared child".into(),
        ));
    }
    let max_threads = options
        .config
        .effective_agent_max_threads(MultiAgentVersion::V2)
        .unwrap_or(usize::MAX);
    let local = destination
        .agent_control_for_config(&options.config)
        .with_session_id(session_id, max_threads);
    let membership = local.runtime.admit_start()?;
    let state = Arc::clone(&destination.state);
    if state
        .threads
        .read()
        .await
        .contains_key(&reservation.thread_id)
    {
        return Err(CodexErr::InvalidRequest(format!(
            "thread {} is already running",
            reservation.thread_id,
        ))
        .into());
    }
    options.reserved_thread_id = Some(reservation.thread_id);
    if let Some(admission) = reservation.turn_admission {
        options.thread_extension_init.insert(admission);
    }
    if let Some(accounting) = reservation.usage_accounting {
        options.thread_extension_init.insert(accounting);
    }
    let mut request = ThreadSpawnRequest::new(
        options,
        auth_manager,
        AgentControlInit::Provided {
            control: reservation.control,
            runtime: local.runtime,
        },
    );
    request.inherited_runtime_auth = Some(runtime_auth);
    request.ready_notification = ThreadReadyNotification::Defer;
    request.parent_thread_id = Some(caller);
    let history_initialization = match &request.options.initial_history {
        InitialHistory::Forked(_) => {
            request.forked_from_thread_id = Some(caller);
            request.fork_persistence = ForkPersistence::CopiedDeferred;
            HistoryInitialization::WarmFork
        }
        InitialHistory::New => HistoryInitialization::New,
        InitialHistory::Resumed(_) | InitialHistory::Cleared => {
            return Err(CodexErr::InvalidRequest(
                "prepared spawn requires new or forked history".into(),
            )
            .into());
        }
    };
    request.inherited_instructions = Some(inheritance.instructions);
    request.inherited_exec_policy = inheritance.exec_policy;
    request.prepared_child = Some(PreparedChildOrigin {
        history_initialization,
        originator: inheritance.originator,
        attachment_source: None,
    });
    let child = Box::pin(state.spawn_thread(request))
        .await
        .map_err(|error| error.with_agent_context(AgentErrorContext::ChildStartup))?;
    let pending = PendingSpawn::new(
        Arc::clone(&state),
        child.thread_id,
        membership,
        SpawnOwner::Host,
    );
    child.thread.emit_thread_ready_lifecycle().await;
    child
        .thread
        .emit_subagent_session_started(client_metadata, /*resumed_created_at*/ None)
        .await;
    if child.session_configured.forked_from_id.is_some() {
        child.thread.ensure_rollout_materialized().await;
    }
    let config = effective_spawn_config(&child.thread).await?;
    let status = child.thread.io.agent_status.borrow().clone();
    let _membership = pending.disarm();
    state.notify_thread_created(child.thread_id);
    Ok(ThreadSpawnResult {
        agent: LiveAgent {
            thread_id: child.thread_id,
            metadata: AgentMetadata {
                agent_id: Some(child.thread_id),
                agent_path: Some(agent_path.clone()),
                agent_nickname: child.thread.session_source.get_nickname(),
                agent_role: child.thread.session_source.get_agent_role(),
            },
            status,
        },
        config,
        task_name: agent_path,
    })
}
