//! App-server session facade used by the TUI event loop.
//!
//! This module owns the typed JSON-RPC calls needed by the TUI and keeps
//! request/response plumbing out of `App` and `ChatWidget`.

mod account;
pub(crate) use account::SessionAccount;
mod background_terminals;
mod daybreak_api_key_compat;
mod events;
pub(crate) mod fork_preparation;
pub(crate) mod fs;
mod history;
pub(crate) mod history_job;
mod models;
mod title_requests;
pub(crate) use title_requests::ManualThreadName;
pub(crate) mod provider_selection;
#[cfg(test)]
#[path = "app_server_session/provider_selection_tests.rs"]
mod provider_selection_tests;
mod read_state;
mod realtime;
mod rollout_history;
pub(crate) mod session_preparation;
mod startup_launch;
mod thread_list;
mod turn_submissions;
pub(crate) mod typed_requests;
pub(crate) use turn_submissions::turn_start_params;

mod operation_capabilities;
pub(crate) use read_state::unread_revision;
mod side_chat_history;
mod thread_deletion;
pub(crate) use thread_deletion::ThreadDeletion;
pub(crate) use thread_deletion::ThreadDeletionError;
pub(crate) mod user_message_submission;
mod web_search;

#[cfg(test)]
#[path = "app_server_session/collaboration_catalog_tests.rs"]
mod collaboration_catalog_tests;

pub(crate) use events::AppServerSessionEvent;
pub(crate) use history::HISTORY_ITEM_PAGE_LIMIT;
pub(crate) use history::HISTORY_ITEM_SCAN_LIMIT;
pub(crate) use history::HistoryContent;
pub(crate) use history::HistoryCursor;
pub(crate) use history::HistoryHydrationScope;
pub(crate) use history::HistoryItem;
pub(crate) use history::HistoryPage;
pub(crate) use history::HistoryResponse;
pub(crate) use history::INITIAL_HISTORY_TURN_LIMIT;
pub(crate) use history::thread_items_page_params;
pub(crate) use side_chat_history::SideChat;
pub(crate) use side_chat_history::side_chat_prompt_preview;
pub(crate) use startup_launch::StartupLaunchChoices;

use crate::app_event::PermissionProfileSelection;
use crate::legacy_core::config::Config;
use crate::local_settings::LocalSettings;
use crate::service_tier_resolution;
use crate::session_state::MessageHistoryMetadata;
use crate::session_state::ThreadSessionState;
use crate::status::StatusAccountDisplay;
use crate::status::plan_type_display_name;
use crate::terminal_visualization_instructions::with_terminal_visualization_instructions;
use codex_app_server_client::AppServerClient;
use codex_app_server_client::AppServerPath;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConfigRequirementsReadResponse;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::Model as ApiModel;
use codex_app_server_protocol::NewThreadModelDefaults;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SessionSource;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadArchiveResponse;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadListResponse;
#[cfg(test)]
use codex_app_server_protocol::ThreadLoadedListParams;
#[cfg(test)]
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadSource;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadStartSource;
use codex_app_server_protocol::ThreadUnarchiveParams;
use codex_app_server_protocol::ThreadUnarchiveResponse;
use codex_app_server_protocol::Turn;
#[cfg(test)]
use codex_app_server_protocol::TurnStartParams;
#[cfg(test)]
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_config::ConfigLayerSource;
use codex_context_fragments::SideConversationPolicy;
use codex_otel::TelemetryAuthMode;
use codex_protocol::ThreadId;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelAvailabilityNux;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelServiceTier;
use codex_protocol::openai_models::ModelUpgrade;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::SubAgentSource;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::LegacyAppPathString;
use codex_utils_path_uri::LegacyAppPathStringError;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;
use codex_utils_path_uri::Platform;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use futures::FutureExt;
use futures::future::Either;
use futures::future::RemoteHandle;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use uuid::Uuid;

pub(crate) const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;
const THREAD_SETTINGS_UPDATE_METHOD: &str = "thread/settings/update";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForkGoalContinuation {
    StartIfIdle,
    DeferUntilNextTurn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForkPermissionMode {
    InheritSaved,
    OverrideFromCurrentConfig,
}

enum EmptyCatalogFallback {
    Error,
    ManagedNewThread,
    DeferredThread(Option<String>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkPresentation {
    Regular,
    SideConversation(SideChatStorage),
}

pub(crate) const ATTACHED_SIDE_CHAT_SOURCE: &str = "tui_side_chat";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SideChatStorage {
    Temporary,
    Attached,
}

impl SideChatStorage {
    fn policy(self) -> SideConversationPolicy {
        match self {
            Self::Temporary => SideConversationPolicy::Temporary,
            Self::Attached => SideConversationPolicy::Attached,
        }
    }

    pub(crate) fn configure_fork(self, config: &mut Config) {
        config.ephemeral = self == Self::Temporary;
        config.developer_instructions = Some(
            self.policy()
                .instructions(config.developer_instructions.as_deref()),
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkConfigSource<'a> {
    Local,
    Session {
        workspace_roots: Option<&'a [PathUri]>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThreadHistorySupport {
    Paginated,
    LegacyOnly,
}

#[derive(Clone, Debug)]
pub(crate) struct ForkName {
    pub name: String,
    pub conflict_thread_ids: Vec<String>,
}

fn bootstrap_request_error(context: &'static str, err: TypedRequestError) -> color_eyre::Report {
    let message = format!("{context}: {err}");
    color_eyre::Report::new(err).wrap_err(message)
}

pub(crate) fn is_method_not_found(error: &TypedRequestError) -> bool {
    let TypedRequestError::Server { method, source } = error else {
        return false;
    };
    // Older servers report an unknown method as an invalid request during deserialization.
    source.code == JSONRPC_METHOD_NOT_FOUND
        || (source.code == JSONRPC_INVALID_REQUEST
            && source.message.contains(method.as_str())
            && (source.message.contains("unknown variant")
                || source.message.contains("unknown method")))
}

pub(crate) fn is_history_pagination_unsupported(source: &JSONRPCErrorError) -> bool {
    if source.code == JSONRPC_METHOD_NOT_FOUND {
        return true;
    }

    if !matches!(
        source.code,
        JSONRPC_INVALID_REQUEST | JSONRPC_INVALID_PARAMS
    ) {
        return false;
    }

    let message = source.message.to_ascii_lowercase();
    [
        "historymode",
        "history mode",
        "excludeturns",
        "exclude turns",
        "thread/turns/list",
        "thread/items/list",
    ]
    .into_iter()
    .any(|field| message.contains(field))
        || (message.contains("paginated")
            && ["unknown variant", "unsupported variant", "invalid enum"]
                .into_iter()
                .any(|error| message.contains(error)))
}

pub(crate) async fn request_thread_start_with_history_fallback(
    request_handle: &AppServerRequestHandle,
    mut request_id: RequestId,
    mut params: ThreadStartParams,
) -> std::result::Result<(ThreadStartResponse, ThreadHistorySupport, bool), TypedRequestError> {
    let mut history_support = ThreadHistorySupport::Paginated;
    loop {
        match request_handle
            .request_typed(ClientRequest::ThreadStart {
                request_id,
                params: params.clone(),
            })
            .await
        {
            Ok(response) => {
                return Ok((
                    response,
                    history_support,
                    request_handle.supports_thread_coordination(),
                ));
            }
            Err(TypedRequestError::Server { source, .. })
                if params.history_mode.is_some() && is_history_pagination_unsupported(&source) =>
            {
                params.history_mode = None;
                history_support = ThreadHistorySupport::LegacyOnly;
                request_id = RequestId::String(format!("legacy-thread-start-{}", Uuid::new_v4()));
            }
            Err(TypedRequestError::Server { source, .. })
                if params.dynamic_tools.is_some()
                    && matches!(
                        source.code,
                        JSONRPC_INVALID_REQUEST | JSONRPC_INVALID_PARAMS
                    )
                    && {
                        let message = source.message.to_ascii_lowercase();
                        ["dynamictools", "dynamic tool", "namespace", "inputschema"]
                            .into_iter()
                            .any(|field| message.contains(field))
                    } =>
            {
                tracing::warn!(
                    error = %source.message,
                    "app server does not support TUI dynamic tools; starting without them"
                );
                params.dynamic_tools = None;
                request_id = RequestId::String(format!("legacy-thread-start-{}", Uuid::new_v4()));
            }
            Err(err) => return Err(err),
        }
    }
}

pub(crate) fn is_thread_settings_update_unsupported(source: &JSONRPCErrorError) -> bool {
    is_method_unsupported(source, THREAD_SETTINGS_UPDATE_METHOD)
}

pub(crate) fn is_method_unsupported(source: &JSONRPCErrorError, method: &str) -> bool {
    source.code == JSONRPC_METHOD_NOT_FOUND
        || (source.code == JSONRPC_INVALID_REQUEST && source.message.contains(method))
}

#[derive(Clone)]
pub(crate) enum BootstrapAccount {
    AwaitingResume,
    Loaded(SessionAccount),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BackendAuthority {
    #[default]
    Unreported,
    Reported(Option<AuthMode>),
}

impl BackendAuthority {
    pub(crate) fn has_codex_backend_auth(self) -> bool {
        matches!(
            self,
            Self::Reported(Some(
                AuthMode::Chatgpt
                    | AuthMode::ChatgptAuthTokens
                    | AuthMode::Headers
                    | AuthMode::AgentIdentity
                    | AuthMode::PersonalAccessToken
            ))
        )
    }
}

/// Data collected during the TUI bootstrap phase that the main event loop
/// needs to configure the UI, telemetry, and initial rate-limit prefetch.
///
/// Rate-limit snapshots are intentionally **not** included here; they are
/// fetched asynchronously after bootstrap returns so that the TUI can render
/// its first frame without waiting for the rate-limit round-trip.
pub(crate) struct AppServerBootstrap {
    pub(crate) duration: Duration,
    pub(crate) account: BootstrapAccount,
    /// Saved threads can defer model discovery until their pinned provider is loaded.
    pub(crate) default_model: Option<String>,
    pub(crate) available_models: Vec<ModelPreset>,
    pub(crate) collaboration_modes: Vec<codex_protocol::config_types::CollaborationModeMask>,
    pub(crate) allowed_approval_policies: Option<Vec<AskForApproval>>,
}

#[derive(Debug)]
pub(crate) struct AttachmentMetadata {
    pagination: Option<history::ThreadHistoryPagination>,
    unread_revision: Option<String>,
    history_support: ThreadHistorySupport,
    pub(crate) subscribed: bool,
}

pub(crate) struct AppServerSession {
    backend_id: crate::backend_id::BackendId,
    /// Owns events and the connection lifetime for live sessions. Request-only views share the
    /// request handle below and have no connection owner.
    connection_owner: Option<AppServerClient>,
    request_handle: AppServerRequestHandle,
    request_id_namespace: Option<Uuid>,
    next_request_id: i64,
    automatic_name_writes: std::sync::Mutex<HashMap<ThreadId, Arc<title_requests::NameGate>>>,
    /// The newest docked replay refresh on this connection.
    pub(crate) pending_side_skills_refresh: Option<Uuid>,
    history_pagination: HashMap<ThreadId, history::ThreadHistoryPagination>,
    pub(crate) attachment_subscribed: Option<bool>,
    pub(crate) focused_conversation: Option<ThreadId>,
    pub(crate) attempted_revision: Option<String>,
    pub(crate) frozen_receipt: Option<(ThreadId, String)>,
    pub(crate) unread_revisions: HashMap<ThreadId, String>,
    pub(crate) task_search: crate::task_mentions::TaskSearch,
    remote_cwd_override: Option<PathBuf>,
    thread_params_mode: ThreadParamsMode,
    /// Explicit CLI provider selection, distinct from resolved configuration defaults.
    pub(crate) model_provider_override: Option<String>,
    history_support: ThreadHistorySupport,
    pub(crate) thread_settings_update_supported: bool,
    default_model: Option<String>,
    available_models: Vec<ModelPreset>,
    prefetched_model_list: Option<RemoteHandle<Result<Vec<ModelPreset>>>>,
    daybreak_api_key_compat: Arc<tokio::sync::Semaphore>,
    managed_new_thread_defaults: Option<NewThreadModelDefaults>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadParamsMode {
    Embedded,
    Remote,
}

/// Determines where model settings come from when resuming a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResumeModelSettings {
    /// Sends the current config's model, provider, and reasoning effort as explicit overrides.
    OverrideFromCurrentConfig,
    /// Omits those overrides so app-server restores the settings saved with the thread.
    RestoreFromThread,
    /// Rejoins a loaded thread without changing any of its existing settings.
    PreserveExistingThread,
    /// Restore the saved settings and reapply the side-chat instructions on cold resume.
    PreserveSideConversation,
}

impl ThreadParamsMode {
    fn workspace_roots_from_config(self, config: &Config) -> Option<Vec<LegacyAppPathString>> {
        self.local_workspace_roots(&config.workspace_roots)
            .map(LegacyAppPathString::from_abs_paths)
    }

    pub(crate) fn local_workspace_roots(
        self,
        roots: &[AbsolutePathBuf],
    ) -> Option<&[AbsolutePathBuf]> {
        // A remote server owns its workspace roots. A client config can only hold local paths.
        match self {
            Self::Embedded => Some(roots),
            Self::Remote => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AppServerStartedThread {
    pub(crate) session: ThreadSessionState,
    pub(crate) turns: Vec<Turn>,
    pub(crate) blocks_direct_input: bool,
    pub(crate) can_accept_text_input: bool,
    pub(crate) task_tools_available: bool,
    pub(crate) history_notice: Option<String>,
}

/// Recognize typed server errors without depending on wording or an older server's behavior.
pub(crate) fn model_provider_error(
    error: &color_eyre::eyre::Report,
) -> Option<codex_app_server_protocol::ModelProviderError> {
    error.chain().find_map(|cause| {
        let codex_app_server_client::TypedRequestError::Server { source, .. } =
            cause.downcast_ref::<codex_app_server_client::TypedRequestError>()?
        else {
            return None;
        };
        serde_json::from_value(source.data.clone()?).ok()
    })
}

pub(crate) fn is_active_writer_error(err: &color_eyre::eyre::Report) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("already has an active writer"))
}

/// Recognizes a draining server's refusal, using the legacy message only when data is absent.
pub(crate) fn is_server_shutting_down_error(error: &JSONRPCErrorError) -> bool {
    error.code == JSONRPC_INVALID_REQUEST
        && match &error.data {
            Some(data) => {
                data.get("reason").and_then(serde_json::Value::as_str) == Some("serverShuttingDown")
            }
            None => error.message == "Server is draining; retry after reconnecting",
        }
}

pub(crate) fn source_agent_path(source: &SessionSource) -> Option<String> {
    match source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_path, .. }) => {
            agent_path.clone().map(String::from)
        }
        _ => None,
    }
}

/// Uses the server capability when available and preserves compatibility with older servers.
pub(crate) fn thread_blocks_direct_input(thread: &Thread) -> bool {
    thread
        .can_accept_direct_input
        .map(|can_accept| !can_accept)
        .unwrap_or_else(|| source_agent_path(&thread.source).is_some())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TurnPermissionsOverride {
    /// Leave the app-server thread's sticky permission profile unchanged.
    Preserve,
    /// Select a named or built-in profile by id.
    ActiveProfile(ActivePermissionProfile),
    /// Apply a user-selected legacy/custom permission profile.
    LegacySandbox(PermissionProfile),
}

#[derive(Debug, thiserror::Error)]
#[error(
    "the selected permission profile cannot be safely represented by the legacy app-server sandbox policy; select a named or legacy-compatible permission profile"
)]
pub(crate) struct UnsupportedLegacyPermissionProfile;

impl AppServerSession {
    pub(crate) fn path_convention(&self) -> Option<PathConvention> {
        Platform::from_platform_os(self.request_handle.platform_os()).path_convention()
    }

    /// Platform of the app-server process, not necessarily its executor.
    pub(crate) fn app_server_platform_family(&self) -> Option<&str> {
        self.connection_owner
            .as_ref()
            .and_then(AppServerClient::platform_family)
    }

    pub(crate) fn app_server_platform_os(&self) -> Option<&str> {
        self.connection_owner
            .as_ref()
            .and_then(AppServerClient::platform_os)
    }

    pub(crate) fn new(
        backend_id: crate::backend_id::BackendId,
        client: AppServerClient,
        thread_params_mode: ThreadParamsMode,
    ) -> Self {
        let request_handle = client.request_handle();
        Self::from_parts(backend_id, Some(client), request_handle, thread_params_mode)
    }

    fn from_parts(
        backend_id: crate::backend_id::BackendId,
        connection_owner: Option<AppServerClient>,
        request_handle: AppServerRequestHandle,
        thread_params_mode: ThreadParamsMode,
    ) -> Self {
        Self {
            backend_id,
            connection_owner,
            request_handle,
            request_id_namespace: None,
            next_request_id: 1,
            automatic_name_writes: std::sync::Mutex::new(HashMap::new()),
            pending_side_skills_refresh: None,
            attachment_subscribed: None,
            history_pagination: HashMap::new(),
            focused_conversation: None,
            attempted_revision: None,
            frozen_receipt: None,
            unread_revisions: HashMap::new(),
            task_search: Default::default(),
            remote_cwd_override: None,
            thread_params_mode,
            model_provider_override: None,
            history_support: ThreadHistorySupport::Paginated,
            thread_settings_update_supported: true,
            default_model: None,
            available_models: Vec::new(),
            prefetched_model_list: None,
            daybreak_api_key_compat: Arc::new(tokio::sync::Semaphore::new(/*permits*/ 1)),
            managed_new_thread_defaults: None,
        }
    }

    pub(crate) fn attachment_view(&self, id: ThreadId) -> Self {
        let mut view = self.request_view();
        view.attachment_subscribed = Some(false);
        if let Some(page) = self.history_pagination.get(&id) {
            view.history_pagination.insert(id, page.clone());
        }
        view
    }

    pub(crate) fn take_attachment_metadata(&mut self, id: ThreadId) -> AttachmentMetadata {
        AttachmentMetadata {
            pagination: self.history_pagination.remove(&id),
            unread_revision: self.unread_revisions.remove(&id),
            history_support: self.history_support,
            subscribed: self.attachment_subscribed == Some(true),
        }
    }

    pub(crate) fn install_attachment_metadata(
        &mut self,
        id: ThreadId,
        metadata: AttachmentMetadata,
        observed_receipt: bool,
    ) {
        if let Some(page) = metadata.pagination {
            self.history_pagination.insert(id, page);
        } else {
            self.history_pagination.remove(&id);
        }
        if metadata.history_support == ThreadHistorySupport::LegacyOnly {
            self.history_support = ThreadHistorySupport::LegacyOnly;
        }
        if !observed_receipt {
            self.unread_revisions.remove(&id);
            if let Some(revision) = metadata.unread_revision {
                self.unread_revisions.insert(id, revision);
            }
        }
    }

    pub(crate) fn backend_id(&self) -> crate::backend_id::BackendId {
        self.backend_id
    }

    /// Creates a request-only view without transferring the live connection's events.
    pub(crate) fn request_view(&self) -> Self {
        let mut view = Self::from_parts(
            self.backend_id,
            /*connection_owner*/ None,
            self.request_handle(),
            self.thread_params_mode,
        );
        view.request_id_namespace = Some(Uuid::new_v4());
        view.remote_cwd_override = self.remote_cwd_override.clone();
        view.model_provider_override = self.model_provider_override.clone();
        view.history_support = self.history_support;
        view.thread_settings_update_supported = self.thread_settings_update_supported;
        view.default_model = self.default_model.clone();
        view.available_models = self.available_models.clone();
        view
    }

    pub(crate) fn with_remote_cwd_override(mut self, remote_cwd_override: Option<PathBuf>) -> Self {
        self.remote_cwd_override = remote_cwd_override;
        self
    }

    pub(crate) fn remote_cwd_override(&self) -> Option<&std::path::Path> {
        self.remote_cwd_override.as_deref()
    }

    pub(crate) fn uses_remote_workspace(&self) -> bool {
        matches!(self.thread_params_mode, ThreadParamsMode::Remote)
    }

    pub(crate) fn uses_embedded_app_server(&self) -> bool {
        matches!(&self.request_handle, AppServerRequestHandle::InProcess(_))
    }

    pub(crate) fn task_tools_available(&self, _thread_id: ThreadId) -> bool {
        self.request_handle.supports_thread_coordination()
    }

    pub(crate) fn codex_home_path(
        &self,
        local_codex_home: &AbsolutePathBuf,
    ) -> Option<AppServerPath> {
        self.connection_owner
            .as_ref()
            .and_then(|client| client.codex_home(local_codex_home))
    }

    pub(crate) fn server_version(&self) -> Option<&str> {
        let Some(AppServerClient::Remote(client)) = &self.connection_owner else {
            return None;
        };
        client.server_version()
    }

    pub(crate) fn server_codex_home(&self) -> Option<&str> {
        let Some(AppServerClient::Remote(client)) = &self.connection_owner else {
            return None;
        };
        client.codex_home()
    }

    pub(crate) fn prefetch_model_list(&mut self) {
        let (request, models) = self.request_model_list(None, None, true).remote_handle();
        tokio::spawn(request);
        self.prefetched_model_list = Some(models);
    }

    fn request_model_list(
        &mut self,
        model_provider: Option<String>,
        thread_id: Option<ThreadId>,
        discover: bool,
    ) -> impl Future<Output = Result<Vec<ModelPreset>>> + use<> {
        let handle = self.request_handle();
        let request_id = self.next_request_id();
        async move {
            if !discover {
                return Ok(Vec::new());
            }
            models::list(handle, request_id, model_provider, thread_id)
                .await
                .map_err(|err| {
                    bootstrap_request_error("model/list failed during TUI bootstrap", err)
                })
        }
    }

    pub(crate) async fn bootstrap(&mut self, config: &Config) -> Result<AppServerBootstrap> {
        self.bootstrap_for_thread(config, /*thread_id*/ None).await
    }

    pub(crate) async fn bootstrap_for_new_thread(
        &mut self,
        config: &Config,
    ) -> Result<AppServerBootstrap> {
        let started_at = Instant::now();
        let account = self.read_bootstrap_account(config, None).await?;
        let mut bootstrap = self
            .bootstrap_with_account_inner(
                config,
                BootstrapAccount::Loaded(account.into()),
                self.explicit_model_provider(config),
                EmptyCatalogFallback::ManagedNewThread,
                None,
            )
            .await?;
        bootstrap.duration = started_at.elapsed();
        Ok(bootstrap)
    }

    /// Bootstraps using a previously read account.
    ///
    /// Callers must discard a prefetched account after authentication, server, or provider changes.
    pub(crate) async fn bootstrap_with_account(
        &mut self,
        config: &Config,
        account: GetAccountResponse,
        model_provider: Option<String>,
    ) -> Result<AppServerBootstrap> {
        self.bootstrap_with_account_inner(
            config,
            BootstrapAccount::Loaded(account.into()),
            model_provider,
            EmptyCatalogFallback::Error,
            None,
        )
        .await
    }

    pub(super) async fn bootstrap_with_account_for_thread(
        &mut self,
        config: &Config,
        account: BootstrapAccount,
        thread: Thread,
    ) -> Result<AppServerBootstrap> {
        let thread_id = ThreadId::from_string(&thread.id)?;
        let fallback = if matches!(
            thread.status,
            codex_app_server_protocol::ThreadStatus::NotLoaded
        ) {
            EmptyCatalogFallback::DeferredThread(thread.model)
        } else {
            EmptyCatalogFallback::Error
        };
        self.bootstrap_with_account_inner(
            config,
            account,
            Some(thread.model_provider),
            fallback,
            Some(thread_id),
        )
        .await
    }

    async fn bootstrap_with_account_inner(
        &mut self,
        config: &Config,
        account: BootstrapAccount,
        model_provider: Option<String>,
        fallback: EmptyCatalogFallback,
        catalog_thread: Option<ThreadId>,
    ) -> Result<AppServerBootstrap> {
        let started_at = Instant::now();
        // A saved thread's account catalog does not exist until resume captures its provider.
        let discover = !matches!(fallback, EmptyCatalogFallback::DeferredThread(_));
        let models = match self.prefetched_model_list.take() {
            Some(models) if discover && model_provider.is_none() && catalog_thread.is_none() => {
                Either::Left(models)
            }
            Some(_) => Either::Right(self.request_model_list(
                model_provider.clone(),
                catalog_thread,
                discover,
            )),
            None => Either::Right(self.request_model_list(
                model_provider.clone(),
                catalog_thread,
                discover,
            )),
        };
        let requirements_request_id = self.next_request_id();
        let (mut available_models, requirements, collaboration_modes) = tokio::try_join!(
            models,
            async {
                match self
                    .request_handle
                    .request_typed::<ConfigRequirementsReadResponse>(
                        ClientRequest::ConfigRequirementsRead {
                            request_id: requirements_request_id,
                            params: None,
                        },
                    )
                    .await
                {
                    Ok(requirements) => Ok(Some(requirements)),
                    Err(error) => Err(bootstrap_request_error(
                        "configRequirements/read failed during TUI bootstrap",
                        error,
                    )),
                }
            },
            async { Ok(crate::collaboration_modes::list(self.request_handle()).await) },
        )?;
        if let Some(requirements) = &requirements {
            crate::service_tier_resolution::constrain_server_service_tiers(
                &mut available_models,
                requirements,
            );
        }
        if !available_models.is_empty()
            && matches!(&account, BootstrapAccount::Loaded(account)
                if matches!(&account.display, Some(StatusAccountDisplay::ApiKey)))
        {
            available_models = self
                .constrain_api_key_models(
                    config,
                    catalog_thread,
                    requirements
                        .as_ref()
                        .and_then(|response| response.requirements.as_ref()),
                    available_models,
                )
                .await;
        }
        let allowed_approval_policies = requirements
            .as_ref()
            .and_then(|response| response.requirements.as_ref())
            .and_then(|requirements| requirements.allowed_approval_policies.clone());
        self.managed_new_thread_defaults = requirements
            .and_then(|response| response.requirements)
            .and_then(|requirements| requirements.models)
            .and_then(|models| models.new_thread);
        let default_model = match &fallback {
            EmptyCatalogFallback::DeferredThread(model) => model.clone(),
            _ => config.model.clone(),
        }
        .or_else(|| {
            available_models
                .iter()
                .find(|model| model.is_default)
                .map(|model| model.model.clone())
        })
        .or_else(|| available_models.first().map(|model| model.model.clone()))
        .or_else(|| match fallback {
            EmptyCatalogFallback::Error | EmptyCatalogFallback::DeferredThread(_) => None,
            EmptyCatalogFallback::ManagedNewThread => self
                .managed_new_thread_defaults
                .as_ref()
                .and_then(|defaults| defaults.model.clone()),
        });
        if discover && default_model.is_none() {
            color_eyre::eyre::bail!(
                "No models are available. Set `model` explicitly or check your model catalog configuration."
            );
        }
        self.default_model.clone_from(&default_model);
        self.available_models = available_models.clone();

        Ok(AppServerBootstrap {
            duration: started_at.elapsed(),
            account,
            default_model,
            available_models,
            collaboration_modes,
            allowed_approval_policies,
        })
    }

    pub(crate) fn managed_new_thread_defaults(&self) -> Option<&NewThreadModelDefaults> {
        self.managed_new_thread_defaults.as_ref()
    }

    pub(crate) fn supports_paginated_history(&self) -> bool {
        self.history_support == ThreadHistorySupport::Paginated
    }

    /// Fetches the current account info without refreshing the auth token.
    ///
    /// Used for login detection; draft identity is resolved separately through
    /// the same account selection as a fresh root conversation.
    pub(crate) async fn read_account(&mut self) -> Result<GetAccountResponse> {
        let account_request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::GetAccount {
                request_id: account_request_id,
                params: GetAccountParams {
                    new_thread: None,
                    thread_id: None,
                    refresh_token: false,
                },
            })
            .await
            .map_err(|err| bootstrap_request_error("account/read failed", err))
    }

    pub(crate) async fn next_event(&mut self) -> Option<AppServerSessionEvent> {
        let event = self.connection_owner.as_mut()?.next_event().await?;
        if let codex_app_server_client::AppServerEvent::ServerNotification(notification) = &event
            && let codex_app_server_protocol::ServerNotification::ThreadReadStateChanged(change) =
                notification.as_ref()
            && let Ok(id) = ThreadId::from_string(&change.thread_id)
        {
            self.remember_read_state(id, Some(change.read_state.clone()));
        }
        Some(AppServerSessionEvent {
            backend_id: self.backend_id,
            event,
        })
    }

    #[cfg(test)]
    pub(crate) async fn start_thread(&mut self, config: &Config) -> Result<AppServerStartedThread> {
        self.start_thread_with_session_start_source(
            &LocalSettings::from(config),
            config,
            /*session_start_source*/ None,
            /*remote_cwd_override*/ None,
            /*selected_profile*/ None,
        )
        .await
    }

    pub(crate) async fn start_thread_with_session_start_source(
        &mut self,
        local_settings: &LocalSettings,
        config: &Config,
        session_start_source: Option<ThreadStartSource>,
        remote_cwd_override: Option<&std::path::Path>,
        selected_profile: Option<&PermissionProfileSelection>,
    ) -> Result<AppServerStartedThread> {
        let session_config = self.session_config_with_effective_service_tier(config);
        let mut params = thread_start_params_from_config(
            &session_config,
            self.thread_params_mode(),
            remote_cwd_override.or(self.remote_cwd_override.as_deref()),
            session_start_source,
        );
        params.model_provider = self.explicit_model_provider(config);
        params.daybreak_enabled = (config.daybreak_enabled && !config.ephemeral).then_some(true);
        session_preparation::apply_selected_start_permissions(&mut params, selected_profile);
        self.start_thread_with_params(local_settings, config, params)
            .await
    }

    async fn start_thread_with_params(
        &mut self,
        local_settings: &LocalSettings,
        config: &Config,
        params: ThreadStartParams,
    ) -> Result<AppServerStartedThread> {
        let prepared = self
            .prepare_start_with_params(local_settings, config, params)
            .run()
            .await;
        self.apply_session_observations(prepared.observations)?;
        prepared.result
    }

    #[cfg(test)]
    pub(crate) async fn fork_thread(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
    ) -> Result<AppServerStartedThread> {
        self.fork_thread_with_permission_mode(
            local_settings,
            config,
            thread_id,
            ForkPermissionMode::InheritSaved,
        )
        .await
    }

    pub(crate) async fn fork_thread_with_permission_mode(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        permission_mode: ForkPermissionMode,
    ) -> Result<AppServerStartedThread> {
        self.fork_thread_at_with_presentation(
            local_settings,
            config,
            thread_id,
            /*last_turn_id*/ None,
            /*before_turn_id*/ None,
            ForkGoalContinuation::StartIfIdle,
            ForkPresentation::Regular,
            /*selected_profile*/ None,
            permission_mode,
            ForkConfigSource::Local,
            /*name*/ None,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "keep local preferences separate while the legacy Config parameter is still required"
    )]
    async fn fork_thread_at_with_presentation(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        last_turn_id: Option<String>,
        before_turn_id: Option<String>,
        goal_continuation: ForkGoalContinuation,
        presentation: ForkPresentation,
        selected_profile: Option<&PermissionProfileSelection>,
        permission_mode: ForkPermissionMode,
        config_source: ForkConfigSource<'_>,
        name: Option<ForkName>,
    ) -> Result<AppServerStartedThread> {
        let prepared = self
            .prepare_fork_with_presentation(
                session_preparation::RequestPolicy::Unbounded,
                local_settings,
                config,
                thread_id,
                last_turn_id,
                before_turn_id,
                goal_continuation,
                presentation,
                selected_profile,
                permission_mode,
                config_source,
                name,
            )
            .run()
            .await;
        self.apply_session_observations(prepared.observations)?;
        prepared.result
    }

    pub(crate) fn thread_params_mode(&self) -> ThreadParamsMode {
        self.thread_params_mode
    }

    fn session_config_with_effective_service_tier(&self, config: &Config) -> Config {
        let Some(model) = config.model.as_deref().or(self.default_model.as_deref()) else {
            return config.clone();
        };
        let local_settings = LocalSettings::from(config);
        let mut session_config = config.clone();
        match service_tier_resolution::service_tier_update_for_core(
            config,
            &local_settings.notices,
            model,
            &self.available_models,
        ) {
            Some(Some(service_tier)) => {
                session_config.service_tier = Some(service_tier);
                session_config.notices.fast_default_opt_out = None;
            }
            Some(None) => {
                session_config.service_tier = Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string());
                session_config.notices.fast_default_opt_out = None;
            }
            None => {
                session_config.service_tier = None;
                session_config.notices.fast_default_opt_out = None;
            }
        }
        session_config
    }

    /// Lists thread ids that the app server currently holds in memory.
    ///
    /// Used by `App::backfill_loaded_subagent_threads` to discover subagent threads that were
    /// spawned before the TUI connected. The caller then fetches full metadata per thread via
    /// `thread_read` and walks the spawn tree.
    #[cfg(test)]
    pub(crate) async fn thread_loaded_list(
        &mut self,
        params: ThreadLoadedListParams,
    ) -> Result<ThreadLoadedListResponse> {
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::ThreadLoadedList { request_id, params })
            .await
            .wrap_err("failed to list loaded threads from app server")
    }

    pub(crate) async fn thread_read(
        &mut self,
        thread_id: ThreadId,
        include_turns: bool,
    ) -> Result<Thread> {
        let (thread, receipt) = history::preparation::read_thread_with_receipt(
            session_preparation::RequestPolicy::Unbounded,
            &self.request_handle(),
            thread_id,
            include_turns,
        )
        .await?;
        if !include_turns {
            self.remember_read_state(thread_id, receipt);
        }
        Ok(thread)
    }

    pub(crate) async fn thread_archive(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadArchiveResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadArchive {
                request_id,
                params: ThreadArchiveParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("failed to archive session")?;
        Ok(())
    }

    pub(crate) async fn thread_delete(&mut self, thread_id: ThreadId) -> Result<()> {
        let deletion = self.prepare_thread_delete(thread_id);
        let request_id = self.next_request_id();
        deletion
            .run(request_id)
            .await
            .map(drop)
            .map_err(|error| match error {
                ThreadDeletionError::Preflight(error) => error,
                ThreadDeletionError::Request(error) => error.into(),
            })
            .wrap_err("failed to delete session")
    }

    pub(crate) async fn thread_unarchive(&mut self, thread_id: ThreadId) -> Result<Thread> {
        let request_id = self.next_request_id();
        let response: ThreadUnarchiveResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadUnarchive {
                request_id,
                params: ThreadUnarchiveParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("failed to unarchive session")?;
        Ok(response.thread)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    async fn turn_start(
        &mut self,
        thread_id: ThreadId,
        turn_trigger: &str,
        client_user_message_id: String,
        items: Vec<UserInput>,
        cwd: PathBuf,
        approval_policy: Option<AskForApproval>,
        approvals_reviewer: Option<codex_app_server_protocol::ApprovalsReviewer>,
        permissions_override: TurnPermissionsOverride,
        workspace_roots: &[AbsolutePathBuf],
        model: String,
        effort: Option<codex_protocol::openai_models::ReasoningEffort>,
        summary: Option<codex_protocol::config_types::ReasoningSummary>,
        service_tier: Option<Option<String>>,
        collaboration_mode: Option<codex_protocol::config_types::CollaborationMode>,
        output_schema: Option<serde_json::Value>,
        cyber_access_program: Option<codex_app_server_protocol::CyberAccessProgram>,
    ) -> Result<TurnStartResponse> {
        let mut params = TurnStartParams {
            thread_id: thread_id.to_string(),
            turn_trigger: Some(turn_trigger.to_string()),
            client_user_message_id: Some(client_user_message_id),
            input: items,
            output_schema,
            cyber_access_program,
            ..TurnStartParams::default()
        };
        let (sandbox_policy, permissions) =
            turn_permissions_overrides(permissions_override, cwd.as_path())?;
        params.cwd = Some(cwd);
        params.runtime_workspace_roots = self
            .thread_params_mode
            .local_workspace_roots(workspace_roots)
            .map(<[_]>::to_vec);
        params.approval_policy = approval_policy;
        params.approvals_reviewer = approvals_reviewer;
        params.sandbox_policy = sandbox_policy;
        params.permissions = permissions;
        params.model = Some(model);
        params.service_tier = service_tier;
        params.effort = effort;
        params.summary = summary;
        params.collaboration_mode = collaboration_mode;
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::TurnStart { request_id, params })
            .await
            .wrap_err("turn/start failed in TUI")
    }

    pub(crate) async fn shutdown(self) -> std::io::Result<()> {
        match self.connection_owner {
            Some(client) => client.shutdown().await,
            None => Ok(()),
        }
    }

    pub(crate) fn request_handle(&self) -> AppServerRequestHandle {
        self.request_handle.clone()
    }

    pub(crate) fn next_request_id(&mut self) -> RequestId {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        self.request_id_namespace.map_or_else(
            || RequestId::Integer(request_id),
            |namespace| RequestId::String(format!("picker-{namespace}-{request_id}")),
        )
    }
}

pub(crate) async fn start_thread_with_request_handle(
    request_handle: AppServerRequestHandle,
    local_settings: &LocalSettings,
    config: Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<PathBuf>,
    model_provider_override: Option<String>,
    launch_choices: StartupLaunchChoices,
) -> Result<AppServerStartedThread> {
    let request_id = RequestId::String(format!("startup-thread-start-{}", Uuid::new_v4()));
    let mut params = thread_start_params_with_reviewer(
        &config,
        thread_params_mode,
        remote_cwd_override.as_deref(),
        /*session_start_source*/ None,
        launch_choices.approvals_reviewer,
        &launch_choices.approval_policy,
    );
    launch_choices.configure(&mut params);
    params.model_provider = model_provider_override.or(params.model_provider);
    params.daybreak_enabled = (config.daybreak_enabled && !config.ephemeral).then_some(true);
    let (response, _history_support, task_tools_available) =
        request_thread_start_with_history_fallback(&request_handle, request_id, params)
            .await
            .map_err(|err| {
                bootstrap_request_error("thread/start failed during TUI bootstrap", err)
            })?;
    let convention = Platform::from_platform_os(request_handle.platform_os()).path_convention();
    let cwd = decode_thread_cwd(&response.cwd, convention)?;
    let mut started = started_thread_from_start_response(
        response,
        local_settings,
        &config,
        thread_params_mode,
        cwd,
    )
    .await?;
    started.task_tools_available = task_tools_available;
    Ok(started)
}

pub(crate) fn workspace_display(
    name: Option<String>,
    routing: Option<codex_app_server_protocol::WorkspaceRouting>,
) -> Option<String> {
    let name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control));
    let id = routing.map(|routing| routing.chatgpt_account_id);
    match (name, id) {
        (Some(name), Some(id)) => Some(format!("{name} ({id})")),
        (name, id) => name.or(id),
    }
}

pub(crate) fn model_preset_from_api_model(model: ApiModel) -> ModelPreset {
    let upgrade = model.upgrade.map(|upgrade_id| {
        let upgrade_info = model.upgrade_info.clone();
        ModelUpgrade {
            id: upgrade_id,
            migration_config_key: model.model.clone(),
            model_link: upgrade_info
                .as_ref()
                .and_then(|info| info.model_link.clone()),
            upgrade_copy: upgrade_info
                .as_ref()
                .and_then(|info| info.upgrade_copy.clone()),
            migration_markdown: upgrade_info.and_then(|info| info.migration_markdown),
            retirement_at: model
                .upgrade_info
                .as_ref()
                .and_then(|info| info.retirement_at)
                .and_then(|retirement_at| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(retirement_at, 0)
                }),
        }
    });

    ModelPreset {
        id: model.id,
        model: model.model,
        display_name: model.display_name,
        description: model.description,
        model_specialty: model.model_specialty,
        default_reasoning_effort: model.default_reasoning_effort,
        supported_reasoning_efforts: model
            .supported_reasoning_efforts
            .into_iter()
            .map(|effort| ReasoningEffortPreset {
                effort: effort.reasoning_effort,
                description: effort.description,
            })
            .collect(),
        supports_personality: false,
        additional_speed_tiers: model.additional_speed_tiers,
        service_tiers: model
            .service_tiers
            .into_iter()
            .map(|service_tier| ModelServiceTier {
                id: service_tier.id,
                name: service_tier.name,
                description: service_tier.description,
            })
            .collect(),
        default_service_tier: model.default_service_tier,
        available_access_programs: model.available_access_programs.map(Into::into),
        is_default: model.is_default,
        upgrade,
        show_in_picker: !model.hidden,
        multi_agent_version: None,
        availability_nux: model.availability_nux.map(|nux| ModelAvailabilityNux {
            message: nux.message,
        }),
        // `model/list` already returns models filtered for the active client/auth context.
        supported_in_api: true,
        input_modalities: model.input_modalities,
    }
}

fn approvals_reviewer_override_from_config(
    config: &Config,
) -> Option<codex_app_server_protocol::ApprovalsReviewer> {
    Some(config.approvals_reviewer.into())
}

fn config_request_overrides_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Option<HashMap<String, serde_json::Value>> {
    let mut session_config = toml::Value::Table(toml::Table::new());
    for layer in config.config_layer_stack.layers_low_to_high() {
        if matches!(&layer.name, ConfigLayerSource::SessionFlags) {
            codex_config::merge_toml_values(&mut session_config, &layer.config);
        }
    }
    let mut overrides: HashMap<_, _> = session_config
        .as_table()
        .into_iter()
        .flatten()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "allow_login_shell"
                    | "default_permissions"
                    | "features"
                    | "network"
                    | "permissions"
                    | "personality"
                    | "sandbox_workspace_write"
                    | "shell_environment_policy"
                    | "suppress_unstable_features_warning"
            )
        })
        .filter_map(|(key, value)| {
            serde_json::to_value(value)
                .ok()
                .map(|value| (key.clone(), value))
        })
        .collect();
    let mut insert = |key: &str, value: Option<String>| {
        if let Some(value) = value {
            overrides.insert(key.to_string(), serde_json::Value::String(value));
        }
    };
    insert(
        "model_reasoning_effort",
        config
            .model_reasoning_effort
            .as_ref()
            .map(std::string::ToString::to_string),
    );
    // Only winning launch choices may replace server defaults or saved thread settings.
    let origins = config.config_layer_stack.origins();
    let effective = config.config_layer_stack.effective_config();
    web_search::apply_launch_override(
        config,
        thread_params_mode,
        &effective,
        &origins,
        &mut overrides,
    );
    let is_launch = |key: &str| {
        origins.get(key).is_some_and(|origin| {
            matches!(
                origin.name,
                ConfigLayerSource::SessionFlags
                    | ConfigLayerSource::User {
                        profile: Some(_),
                        ..
                    }
            )
        })
    };
    for key in ["model_reasoning_summary", "model_verbosity"] {
        if is_launch(key) {
            overrides.insert(key.to_string(), serde_json::json!(effective[key]));
        }
    }
    if is_launch("features.concurrent_reasoning_summaries") {
        overrides
            .entry("features".to_string())
            .or_insert_with(|| serde_json::json!({}))["concurrent_reasoning_summaries"] =
            serde_json::json!(effective["features"]["concurrent_reasoning_summaries"]);
    }
    if config.bypass_hook_trust {
        overrides.insert("bypass_hook_trust".to_string(), true.into());
    }
    Some(overrides)
}

fn remove_permission_config_overrides(config: &mut Option<HashMap<String, serde_json::Value>>) {
    if let Some(overrides) = config.as_mut() {
        for key in [
            "default_permissions",
            "permissions",
            "network",
            "sandbox_workspace_write",
        ] {
            overrides.remove(key);
        }
    }
    if config.as_ref().is_some_and(HashMap::is_empty) {
        *config = None;
    }
}

fn service_tier_override_from_config(config: &Config) -> Option<Option<String>> {
    let local_settings = LocalSettings::from(config);
    config.service_tier.clone().map(Some).or_else(|| {
        (local_settings.notices.fast_default_opt_out == Some(true))
            .then(|| Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()))
    })
}

fn sandbox_mode_from_permission_profile(
    permission_profile: &PermissionProfile,
    cwd: &std::path::Path,
) -> Option<codex_app_server_protocol::SandboxMode> {
    match permission_profile {
        PermissionProfile::Disabled => {
            Some(codex_app_server_protocol::SandboxMode::DangerFullAccess)
        }
        PermissionProfile::External { .. } => None,
        PermissionProfile::Managed { .. } => {
            let file_system_policy = permission_profile.file_system_sandbox_policy();
            if file_system_policy.has_full_disk_write_access() {
                permission_profile
                    .network_sandbox_policy()
                    .is_enabled()
                    .then_some(codex_app_server_protocol::SandboxMode::DangerFullAccess)
            } else if file_system_policy.can_write_local_path_with_cwd(cwd, cwd) {
                Some(codex_app_server_protocol::SandboxMode::WorkspaceWrite)
            } else {
                Some(codex_app_server_protocol::SandboxMode::ReadOnly)
            }
        }
    }
}

pub(crate) fn permission_profile_is_safely_represented_by_sandbox_mode(
    permission_profile: &PermissionProfile,
    cwd: &std::path::Path,
) -> bool {
    let Some(sandbox_mode) = sandbox_mode_from_permission_profile(permission_profile, cwd) else {
        return false;
    };
    let projected_profile = match sandbox_mode {
        codex_app_server_protocol::SandboxMode::ReadOnly => PermissionProfile::read_only(),
        codex_app_server_protocol::SandboxMode::WorkspaceWrite => {
            PermissionProfile::from_runtime_permissions(
                &FileSystemSandboxPolicy::workspace_write(
                    &[],
                    /*exclude_tmpdir_env_var*/ false,
                    /*exclude_slash_tmp*/ false,
                )
                .with_workspace_write_cwd(cwd),
                NetworkSandboxPolicy::Restricted,
            )
        }
        codex_app_server_protocol::SandboxMode::DangerFullAccess => PermissionProfile::Disabled,
    };
    permission_profile.network_sandbox_policy() == projected_profile.network_sandbox_policy()
        && permission_profile
            .file_system_sandbox_policy()
            .is_semantically_equivalent_to(&projected_profile.file_system_sandbox_policy(), cwd)
}

fn permission_profile_id_from_active_profile(active: ActivePermissionProfile) -> String {
    active.id
}

pub(crate) fn thread_started_with_read_only_sandbox(response: &ThreadStartResponse) -> bool {
    matches!(
        response.sandbox,
        codex_app_server_protocol::SandboxPolicy::ReadOnly { .. }
    )
}

pub(crate) fn turn_permissions_overrides(
    permissions_override: TurnPermissionsOverride,
    cwd: &std::path::Path,
) -> Result<(
    Option<codex_app_server_protocol::SandboxPolicy>,
    Option<String>,
)> {
    Ok(match permissions_override {
        TurnPermissionsOverride::Preserve => (None, None),
        TurnPermissionsOverride::ActiveProfile(active_permission_profile) => (
            None,
            Some(permission_profile_id_from_active_profile(
                active_permission_profile,
            )),
        ),
        TurnPermissionsOverride::LegacySandbox(permission_profile) => {
            let policy = permission_profile
                .to_legacy_sandbox_policy(cwd)
                .map_err(|_| UnsupportedLegacyPermissionProfile)?;
            let projected_profile =
                PermissionProfile::from_legacy_sandbox_policy_for_cwd(&policy, cwd);
            if !permission_profile
                .file_system_sandbox_policy()
                .is_semantically_equivalent_to(&projected_profile.file_system_sandbox_policy(), cwd)
            {
                return Err(UnsupportedLegacyPermissionProfile.into());
            }
            (Some(policy.into()), None)
        }
    })
}

fn permissions_selection_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Option<String> {
    if matches!(thread_params_mode, ThreadParamsMode::Remote) {
        return None;
    }

    config
        .permissions
        .active_permission_profile()
        .map(permission_profile_id_from_active_profile)
}

pub(crate) fn thread_start_params_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
    session_start_source: Option<ThreadStartSource>,
) -> ThreadStartParams {
    thread_start_params_with_reviewer(
        config,
        thread_params_mode,
        remote_cwd_override,
        session_start_source,
        startup_launch::StartupApprovalsReviewer::Config,
        &startup_launch::StartupApprovalPolicy::ResolvedConfig,
    )
}

fn thread_start_params_with_reviewer(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
    session_start_source: Option<ThreadStartSource>,
    reviewer_source: startup_launch::StartupApprovalsReviewer,
    policy_source: &startup_launch::StartupApprovalPolicy,
) -> ThreadStartParams {
    let permissions = permissions_selection_from_config(config, thread_params_mode);
    let sandbox = permissions
        .is_none()
        .then(|| {
            sandbox_mode_from_permission_profile(
                &config.permissions.effective_permission_profile(),
                config.cwd.as_path(),
            )
        })
        .flatten();
    ThreadStartParams {
        model: config.model.clone(),
        model_provider: provider_selection::explicit_provider(config),
        service_tier: service_tier_override_from_config(config),
        cwd: thread_cwd_from_config(config, thread_params_mode, remote_cwd_override),
        runtime_workspace_roots: thread_params_mode.workspace_roots_from_config(config),
        approval_policy: match policy_source {
            startup_launch::StartupApprovalPolicy::ResolvedConfig => {
                Some(config.permissions.approval_policy.value().into())
            }
            startup_launch::StartupApprovalPolicy::Invocation(policy) => *policy,
            startup_launch::StartupApprovalPolicy::ConfigOverride(_) => None,
        },
        approvals_reviewer: match reviewer_source {
            startup_launch::StartupApprovalsReviewer::Config => {
                approvals_reviewer_override_from_config(config)
            }
            startup_launch::StartupApprovalsReviewer::Invocation(reviewer) => reviewer,
        },
        sandbox,
        permissions,
        config: config_request_overrides_from_config(config, thread_params_mode),
        ephemeral: Some(config.ephemeral),
        history_mode: (!config.ephemeral).then_some(ThreadHistoryMode::Paginated),
        session_start_source,
        thread_source: Some(ThreadSource::User),
        developer_instructions: with_terminal_visualization_instructions(
            config, /*control_instructions*/ None,
        ),
        ..ThreadStartParams::default()
    }
}

fn thread_resume_params_from_config(
    config: Config,
    thread_id: ThreadId,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
    model_settings: ResumeModelSettings,
    permission_overrides: crate::resume_permissions::ResumePermissions,
) -> ThreadResumeParams {
    if matches!(
        model_settings,
        ResumeModelSettings::PreserveExistingThread | ResumeModelSettings::PreserveSideConversation
    ) {
        return ThreadResumeParams {
            thread_id: thread_id.to_string(),
            developer_instructions: if model_settings
                == ResumeModelSettings::PreserveSideConversation
            {
                with_terminal_visualization_instructions(
                    &config,
                    config.developer_instructions.clone(),
                )
            } else {
                None
            },
            ..ThreadResumeParams::default()
        };
    }
    let permissions = permissions_selection_from_config(&config, thread_params_mode);
    let sandbox = permissions
        .is_none()
        .then(|| {
            sandbox_mode_from_permission_profile(
                &config.permissions.effective_permission_profile(),
                config.cwd.as_path(),
            )
        })
        .flatten();
    let mut config_overrides = config_request_overrides_from_config(&config, thread_params_mode);
    if model_settings == ResumeModelSettings::RestoreFromThread
        && let Some(overrides) = config_overrides.as_mut()
    {
        overrides.remove("model_reasoning_effort");
        if overrides.is_empty() {
            config_overrides = None;
        }
    }
    let (model, model_provider) = match model_settings {
        ResumeModelSettings::OverrideFromCurrentConfig => (
            config.model.clone(),
            provider_selection::explicit_provider(&config),
        ),
        ResumeModelSettings::RestoreFromThread
        | ResumeModelSettings::PreserveExistingThread
        | ResumeModelSettings::PreserveSideConversation => (None, None),
    };
    let mut params = ThreadResumeParams {
        thread_id: thread_id.to_string(),
        model,
        model_provider,
        service_tier: service_tier_override_from_config(&config),
        cwd: thread_cwd_from_config(&config, thread_params_mode, remote_cwd_override),
        runtime_workspace_roots: thread_params_mode.workspace_roots_from_config(&config),
        approval_policy: Some(config.permissions.approval_policy.value().into()),
        approvals_reviewer: approvals_reviewer_override_from_config(&config),
        sandbox,
        permissions,
        config: config_overrides,
        developer_instructions: with_terminal_visualization_instructions(
            &config, /*control_instructions*/ None,
        ),
        ..ThreadResumeParams::default()
    };
    if thread_params_mode == ThreadParamsMode::Remote || !permission_overrides.approval_policy {
        params.approval_policy = None;
    }
    if thread_params_mode == ThreadParamsMode::Remote || !permission_overrides.approvals_reviewer {
        params.approvals_reviewer = None;
    }
    if thread_params_mode == ThreadParamsMode::Remote || !permission_overrides.profile {
        params.sandbox = None;
        params.permissions = None;
        remove_permission_config_overrides(&mut params.config);
    }
    if !permission_overrides.workspace_roots {
        params.runtime_workspace_roots = None;
    }
    params
}

fn thread_fork_params_from_config(
    config: Config,
    thread_id: ThreadId,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
) -> ThreadForkParams {
    let permissions = permissions_selection_from_config(&config, thread_params_mode);
    let sandbox = permissions
        .is_none()
        .then(|| {
            sandbox_mode_from_permission_profile(
                &config.permissions.effective_permission_profile(),
                config.cwd.as_path(),
            )
        })
        .flatten();
    ThreadForkParams {
        thread_id: thread_id.to_string(),
        model: config.model.clone(),
        model_provider: provider_selection::explicit_provider(&config),
        service_tier: service_tier_override_from_config(&config),
        cwd: thread_cwd_from_config(&config, thread_params_mode, remote_cwd_override),
        runtime_workspace_roots: thread_params_mode.workspace_roots_from_config(&config),
        approval_policy: Some(config.permissions.approval_policy.value().into()),
        approvals_reviewer: approvals_reviewer_override_from_config(&config),
        sandbox,
        permissions,
        config: config_request_overrides_from_config(&config, thread_params_mode),
        base_instructions: config.base_instructions.clone().filter(|_| {
            !matches!(
                config.base_instructions_provenance,
                Some(BaseInstructionsProvenance::Model { .. })
            )
        }),
        developer_instructions: with_terminal_visualization_instructions(
            &config,
            config.developer_instructions.clone(),
        ),
        ephemeral: config.ephemeral,
        thread_source: Some(ThreadSource::User),
        ..ThreadForkParams::default()
    }
}

fn thread_cwd_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
) -> Option<String> {
    match thread_params_mode {
        ThreadParamsMode::Embedded => Some(config.cwd.to_string_lossy().to_string()),
        ThreadParamsMode::Remote => {
            remote_cwd_override.map(|cwd| cwd.to_string_lossy().to_string())
        }
    }
}

async fn started_thread_from_start_response(
    response: ThreadStartResponse,
    local_settings: &LocalSettings,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    cwd: PathUri,
) -> Result<AppServerStartedThread> {
    let blocks_direct_input = thread_blocks_direct_input(&response.thread);
    let can_accept_text_input = response.thread.can_accept_text_input == Some(true);
    let session = thread_session_state_from_thread_start_response(
        &response,
        local_settings,
        config,
        thread_params_mode,
        cwd,
    )
    .await
    .map_err(color_eyre::eyre::Report::msg)?;
    Ok(AppServerStartedThread {
        session,
        turns: response.thread.turns,
        blocks_direct_input,
        can_accept_text_input,
        history_notice: None,
        task_tools_available: false,
    })
}

async fn started_thread_from_resume_response(
    response: ThreadResumeResponse,
    local_settings: &LocalSettings,
    cwd_convention: Option<PathConvention>,
) -> Result<AppServerStartedThread> {
    let blocks_direct_input = thread_blocks_direct_input(&response.thread);
    let can_accept_text_input = response.thread.can_accept_text_input == Some(true);
    let session =
        thread_session_state_from_thread_resume_response(&response, local_settings, cwd_convention)
            .await
            .map_err(color_eyre::eyre::Report::msg)?;
    Ok(AppServerStartedThread {
        session,
        turns: response.thread.turns,
        blocks_direct_input,
        can_accept_text_input,
        history_notice: None,
        task_tools_available: false,
    })
}

async fn started_thread_from_fork_response(
    response: ThreadForkResponse,
    local_settings: &LocalSettings,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    cwd: PathUri,
) -> Result<AppServerStartedThread> {
    let blocks_direct_input = thread_blocks_direct_input(&response.thread);
    let can_accept_text_input = response.thread.can_accept_text_input == Some(true);
    let session = thread_session_state_from_thread_fork_response(
        &response,
        local_settings,
        config,
        thread_params_mode,
        cwd,
    )
    .await
    .map_err(color_eyre::eyre::Report::msg)?;
    Ok(AppServerStartedThread {
        session,
        turns: response.thread.turns,
        blocks_direct_input,
        can_accept_text_input,
        history_notice: None,
        task_tools_available: false,
    })
}

async fn thread_session_state_from_thread_start_response(
    response: &ThreadStartResponse,
    local_settings: &LocalSettings,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    cwd: PathUri,
) -> Result<ThreadSessionState, String> {
    let permission_profile = display_permission_profile_from_thread_response(
        &response.sandbox,
        &cwd,
        config,
        thread_params_mode,
    )?;
    let mut session = thread_session_state_from_thread_response(
        &response.thread.id,
        response.thread.environments.as_deref(),
        response.thread.forked_from_id.clone(),
        response.thread.name.clone(),
        response.thread.path.clone(),
        response.model.clone(),
        response.model_provider.clone(),
        response.service_tier.clone(),
        response.approval_policy,
        response.approvals_reviewer.to_core(),
        permission_profile,
        response.active_permission_profile.clone().map(Into::into),
        cwd,
        response.runtime_workspace_roots.clone(),
        response.instruction_source_path_uris(),
        response.reasoning_effort.clone(),
        local_settings,
    )
    .await?;
    session.daybreak_enabled = response
        .thread
        .daybreak_enabled
        .unwrap_or(response.thread.ephemeral && config.daybreak_enabled);
    Ok(session)
}

async fn thread_session_state_from_thread_resume_response(
    response: &ThreadResumeResponse,
    local_settings: &LocalSettings,
    cwd_convention: Option<PathConvention>,
) -> Result<ThreadSessionState, String> {
    // The server owns restored rules. This legacy projection is for display only;
    // normal turns do not send it back as an execution override.
    let cwd = decode_thread_cwd(&response.cwd, cwd_convention).map_err(|err| err.to_string())?;
    let permission_profile = response
        .sandbox
        .permission_profile_for_cwd(&cwd)
        .map_err(|err| err.to_string())?;
    let mut session = thread_session_state_from_thread_response(
        &response.thread.id,
        response.thread.environments.as_deref(),
        response.thread.forked_from_id.clone(),
        response.thread.name.clone(),
        response.thread.path.clone(),
        response.model.clone(),
        response.model_provider.clone(),
        response.service_tier.clone(),
        response.approval_policy,
        response.approvals_reviewer.to_core(),
        permission_profile,
        response.active_permission_profile.clone().map(Into::into),
        cwd,
        response.runtime_workspace_roots.clone(),
        response.instruction_source_path_uris(),
        response.reasoning_effort.clone(),
        local_settings,
    )
    .await?;
    session.collaboration_mode = response.collaboration_mode.clone().map(Box::new);
    session.daybreak_enabled = response.thread.daybreak_enabled.unwrap_or(false);
    Ok(session)
}

async fn thread_session_state_from_thread_fork_response(
    response: &ThreadForkResponse,
    local_settings: &LocalSettings,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    cwd: PathUri,
) -> Result<ThreadSessionState, String> {
    let permission_profile = display_permission_profile_from_thread_response(
        &response.sandbox,
        &cwd,
        config,
        thread_params_mode,
    )?;
    let mut session = thread_session_state_from_thread_response(
        &response.thread.id,
        response.thread.environments.as_deref(),
        response.thread.forked_from_id.clone(),
        response.thread.name.clone(),
        response.thread.path.clone(),
        response.model.clone(),
        response.model_provider.clone(),
        response.service_tier.clone(),
        response.approval_policy,
        response.approvals_reviewer.to_core(),
        permission_profile,
        response.active_permission_profile.clone().map(Into::into),
        cwd,
        response.runtime_workspace_roots.clone(),
        response.instruction_source_path_uris(),
        response.reasoning_effort.clone(),
        local_settings,
    )
    .await?;
    session.daybreak_enabled = response.thread.daybreak_enabled.unwrap_or(false);
    Ok(session)
}

fn display_permission_profile_from_thread_response(
    sandbox: &codex_app_server_protocol::SandboxPolicy,
    cwd: &PathUri,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Result<PermissionProfile, String> {
    match thread_params_mode {
        ThreadParamsMode::Embedded
            if matches!(
                config.permissions.effective_permission_profile(),
                PermissionProfile::Disabled
            ) && !matches!(
                sandbox,
                codex_app_server_protocol::SandboxPolicy::DangerFullAccess
            ) =>
        {
            sandbox
                .permission_profile_for_cwd(cwd)
                .map_err(|err| err.to_string())
        }
        ThreadParamsMode::Embedded => Ok(config.permissions.effective_permission_profile()),
        ThreadParamsMode::Remote => sandbox
            .permission_profile_for_cwd(cwd)
            .map_err(|err| err.to_string()),
    }
}

pub(crate) fn decode_thread_cwd(
    cwd: &LegacyAppPathString,
    convention: Option<PathConvention>,
) -> Result<PathUri, LegacyAppPathStringError> {
    match convention {
        Some(convention) => cwd.to_path_uri(convention),
        None => PathUri::try_from(cwd.clone()),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "session mapping keeps explicit fields"
)]
async fn thread_session_state_from_thread_response(
    thread_id: &str,
    environments: Option<&[codex_app_server_protocol::ThreadEnvironment]>,
    forked_from_id: Option<String>,
    thread_name: Option<String>,
    rollout_path: Option<PathBuf>,
    model: String,
    model_provider_id: String,
    service_tier: Option<String>,
    approval_policy: AskForApproval,
    approvals_reviewer: codex_protocol::config_types::ApprovalsReviewer,
    permission_profile: PermissionProfile,
    active_permission_profile: Option<ActivePermissionProfile>,
    cwd: PathUri,
    runtime_workspace_roots: Vec<LegacyAppPathString>,
    instruction_source_paths: Vec<PathUri>,
    reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    local_settings: &LocalSettings,
) -> Result<ThreadSessionState, String> {
    let thread_id = ThreadId::from_string(thread_id)
        .map_err(|err| format!("thread id `{thread_id}` is invalid: {err}"))?;
    let cwd_convention = cwd.infer_path_convention();
    let forked_from_id = forked_from_id
        .as_deref()
        .map(ThreadId::from_string)
        .transpose()
        .map_err(|err| format!("forked_from_id is invalid: {err}"))?;
    let history_config = codex_message_history::HistoryConfig::new(
        local_settings.codex_home.clone(),
        &local_settings.history,
    );
    let (log_id, entry_count) = codex_message_history::history_metadata(&history_config).await;
    Ok(ThreadSessionState {
        read_only_history: false,
        thread_settings_confirmed: true,
        daybreak_enabled: false,
        windows_sandbox_host: crate::windows_sandbox::host_from_environments(environments),
        primary_environment_cwd: crate::session_state::primary_environment_cwd(
            environments.unwrap_or_default(),
        ),
        thread_id,
        forked_from_id,
        fork_parent_title: None,
        thread_name,
        model,
        model_provider_id,
        service_tier,
        approval_policy,
        approvals_reviewer,
        permission_profile,
        active_permission_profile,
        cwd,
        runtime_workspace_roots: runtime_workspace_roots
            .iter()
            .map(|root| decode_thread_cwd(root, cwd_convention))
            .collect::<Result<_, _>>()
            .map_err(|err| err.to_string())?,
        instruction_source_paths,
        reasoning_effort,
        collaboration_mode: None,
        message_history: Some(MessageHistoryMetadata {
            log_id,
            entry_count,
        }),
        network_proxy: None,
        rollout_path,
    })
}

pub(crate) fn app_server_rate_limit_snapshots(
    response: GetAccountRateLimitsResponse,
) -> Vec<RateLimitSnapshot> {
    let primary_limit_id = response.rate_limits.limit_id.clone();
    let mut snapshots = vec![response.rate_limits];
    if let Some(by_limit_id) = response.rate_limits_by_limit_id {
        snapshots.extend(by_limit_id.into_iter().filter_map(|(limit_id, snapshot)| {
            if primary_limit_id.as_deref().is_some_and(|primary_limit_id| {
                primary_limit_id == limit_id
                    || Some(primary_limit_id) == snapshot.limit_id.as_deref()
            }) {
                None
            } else {
                Some(snapshot)
            }
        }));
    }
    snapshots
}

#[cfg(test)]
#[path = "app_server_session/reasoning_defaults_tests.rs"]
mod reasoning_defaults_tests;

#[cfg(test)]
#[path = "app_server_session/workspace_roots_tests.rs"]
mod workspace_roots_tests;

#[cfg(test)]
#[path = "app_server_session/prompt_history_tests.rs"]
mod prompt_history_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy_core::config::ConfigBuilder;
    use crate::legacy_core::config::ConfigOverrides;
    use app_test_support::create_fake_paginated_rollout;
    use app_test_support::create_fake_rollout;
    use codex_app_server_protocol::ThreadItem;
    use codex_app_server_protocol::ThreadStatus;
    use codex_app_server_protocol::Turn;
    use codex_app_server_protocol::TurnStatus;
    use codex_features::Feature;
    use codex_protocol::config_types::ReasoningSummary;
    use codex_protocol::config_types::ServiceTier;
    use codex_protocol::config_types::Verbosity;
    use codex_protocol::config_types::WebSearchMode;
    use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_READ_ONLY;
    use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
    use codex_protocol::models::ManagedFileSystemPermissions;
    use codex_protocol::openai_models::ModelAccessPrograms;
    use codex_protocol::openai_models::ModelServiceTier;
    use codex_protocol::openai_models::ReasoningEffort;
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemPath;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::FileSystemSpecialPath;
    use codex_protocol::permissions::NetworkSandboxPolicy;
    use codex_protocol::turn_input::CyberAccessProgram;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;
    use codex_utils_path_uri::LegacyAppPathString;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    async fn build_config(temp_dir: &TempDir) -> Config {
        ConfigBuilder::default()
            .codex_home(temp_dir.path().to_path_buf())
            .build()
            .await
            .expect("config should build")
    }

    #[tokio::test]
    async fn picker_views_use_distinct_request_id_namespaces() {
        let home = tempfile::tempdir().expect("tempdir");
        let config = build_config(&home).await;
        let mut session = crate::start_embedded_app_server_for_picker(&config)
            .await
            .unwrap();
        let mut first = session.request_view();
        let mut second = session.request_view();

        let first_id = first.next_request_id();
        assert_ne!(session.next_request_id(), first_id);
        assert_ne!(first_id, second.next_request_id());
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn picker_view_shutdown_preserves_live_event_stream() {
        let home = tempfile::tempdir().expect("tempdir");
        let config = build_config(&home).await;
        let mut session = crate::start_embedded_app_server_for_picker(&config)
            .await
            .unwrap();
        let backend_id = session.backend_id();

        session.request_view().shutdown().await.unwrap();
        let started = session.start_thread(&config).await.unwrap();
        let thread_id = started.session.thread_id.to_string();
        let event = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = session.next_event().await.expect("original connection stays live");
            if matches!(&event.event,
                codex_app_server_client::AppServerEvent::ServerNotification(notification)
                    if matches!(notification.as_ref(), codex_app_server_protocol::ServerNotification::ThreadStarted(started)
                        if started.thread.id == thread_id))
            {
                break event;
            }
        }
    }).await.expect("thread-start notification must reach the original connection");
        assert_eq!(event.backend_id, backend_id);
        assert!(matches!(
            event.event,
            codex_app_server_client::AppServerEvent::ServerNotification(_)
        ));
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn bootstrap_projects_resolved_account_without_another_account_read() -> Result<()> {
        let codex_home = tempfile::tempdir()?;
        let mut config = build_config(&codex_home).await;
        let mut catalog = codex_models_manager::bundled_models_response()?;
        for model in &mut catalog.models {
            model.available_access_programs = Some(ModelAccessPrograms {
                daybreak_availability: Default::default(),
                cyber: vec![
                    CyberAccessProgram::Standard,
                    CyberAccessProgram::DaybreakBlue,
                ],
            });
        }
        let catalog_path = codex_home.path().join("models.json");
        std::fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
        let catalog_config = format!(
            "model_catalog_json = {}\n",
            toml::Value::String(catalog_path.display().to_string()),
        );
        std::fs::write(codex_home.path().join("config.toml"), &catalog_config)?;
        config.model_catalog = Some(catalog);
        let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
        let next_request_id = app_server.next_request_id;
        let account = GetAccountResponse {
            display_name: None,
            account_id: None,
            user_id: None,
            workspace_routing: None,
            account: Some(Account::Chatgpt {
                email: Some("teammate@openai.com".to_string()),
                plan_type: codex_protocol::account::PlanType::Plus,
                workspace_name: None,
            }),
            requires_openai_auth: true,
        };

        let bootstrap = app_server
            .bootstrap_with_account(&config, account, /*model_provider*/ None)
            .await?;

        assert_eq!(app_server.next_request_id, next_request_id + 2);
        let BootstrapAccount::Loaded(account) = bootstrap.account else {
            panic!("loaded bootstrap account expected");
        };
        assert_eq!(
            (
                account.email(),
                account.auth_mode(),
                account.plan,
                account.has_chatgpt_account(),
            ),
            (
                Some("teammate@openai.com"),
                Some(TelemetryAuthMode::Chatgpt),
                Some(codex_protocol::account::PlanType::Plus),
                true,
            )
        );

        // Account-specific catalog filtering at startup and before/after thread creation.
        let models = bootstrap.available_models;
        assert!(
            models
                .iter()
                .any(|model| model.available_access_programs.is_some())
        );
        let client_home = tempfile::tempdir()?;
        let key = "features.api_key_cyber_access_programs";
        for (server_enabled, cli_override, forwarding_enabled) in [
            (false, None, false),
            (true, None, true),
            (false, Some(true), true),
            (true, Some(false), false),
        ] {
            std::fs::write(
                codex_home.path().join("config.toml"),
                format!(
                    "{catalog_config}[features]\napi_key_cyber_access_programs = {server_enabled}\n"
                ),
            )?;
            let client_config = ConfigBuilder::default()
                .codex_home(client_home.path().to_path_buf())
                .cli_overrides(
                    cli_override
                        .into_iter()
                        .map(|value| (key.into(), value.into()))
                        .collect(),
                )
                .build()
                .await?;
            let api_key = app_server
                .bootstrap_with_account(
                    &client_config,
                    GetAccountResponse {
                        account_id: None,
                        display_name: None,
                        user_id: None,
                        account: Some(Account::ApiKey {}),
                        requires_openai_auth: true,
                        workspace_routing: None,
                    },
                    /*model_provider*/ None,
                )
                .await?;
            let mut expected = models.clone();
            if !forwarding_enabled {
                for model in &mut expected {
                    model.available_access_programs = None;
                }
            }
            assert_eq!(api_key.available_models, expected);
            let thread = app_server.start_thread(&client_config).await?;
            let refreshed = app_server
                .constrain_api_key_models(
                    &client_config,
                    Some(thread.session.thread_id),
                    /*requirements*/ None,
                    models.clone(),
                )
                .await;
            assert_eq!(refreshed, expected);
        }
        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_reads_new_thread_account() -> Result<()> {
        let codex_home = tempfile::tempdir()?;
        let config = build_config(&codex_home).await;
        let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
        let next_request_id = app_server.next_request_id;

        app_server.bootstrap(&config).await?;

        assert_eq!(app_server.next_request_id, next_request_id + 3);
        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_empty_catalog_reports_configuration_error() -> Result<()> {
        let codex_home = tempfile::tempdir()?;
        let mut config = build_config(&codex_home).await;
        config.model = None;
        let catalog = codex_protocol::openai_models::ModelsResponse::default();
        let catalog_path = codex_home.path().join("models.json");
        std::fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
        std::fs::write(
            codex_home.path().join("config.toml"),
            format!(
                "model_catalog_json = {}\n",
                toml::Value::String(catalog_path.display().to_string()),
            ),
        )?;
        config.model_catalog = Some(catalog);
        let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;

        let error = app_server
            .bootstrap(&config)
            .await
            .err()
            .expect("bootstrap requires a model");
        let message = error.to_string();
        assert!(
            message.starts_with("account/read failed during TUI bootstrap:"),
            "{message}"
        );
        assert!(message.contains("model_catalog_json path"), "{message}");
        assert!(
            message.contains(&catalog_path.display().to_string()),
            "{message}"
        );
        assert!(
            message.ends_with("must contain at least one model (code -32600)"),
            "{message}"
        );

        app_server.shutdown().await?;
        Ok(())
    }

    fn rate_limit_snapshot(limit_id: &str) -> RateLimitSnapshot {
        RateLimitSnapshot {
            limit_id: Some(limit_id.to_string()),
            limit_name: None,
            normal_model_slug: None,
            primary: Some(codex_app_server_protocol::RateLimitWindow {
                used_percent: 0,
                window_duration_mins: Some(10_080),
                resets_at: None,
            }),
            secondary: None,
            credits: None,
            individual_limit: None,
            spend_control_reached: None,
            plan_type: None,
            rate_limit_reached_type: None,
        }
    }

    fn api_model_with_upgrade_retirement_at(retirement_at: Option<i64>) -> ApiModel {
        ApiModel {
            id: "model-id".to_string(),
            model: "current-model".to_string(),
            upgrade: Some("replacement-model".to_string()),
            upgrade_info: Some(codex_app_server_protocol::ModelUpgradeInfo {
                model: "replacement-model".to_string(),
                upgrade_copy: None,
                model_link: None,
                migration_markdown: None,
                retirement_at,
            }),
            availability_nux: None,
            display_name: "Current model".to_string(),
            description: "A test model".to_string(),
            model_specialty: None,
            hidden: false,
            supported_reasoning_efforts: Vec::new(),
            default_reasoning_effort: ReasoningEffort::Medium,
            input_modalities: Vec::new(),
            supports_personality: false,
            multi_agent_version: None,
            additional_speed_tiers: Vec::new(),
            service_tiers: Vec::new(),
            default_service_tier: None,
            available_access_programs: None,
            is_default: false,
        }
    }

    #[test]
    fn model_preset_from_api_model_preserves_upgrade_retirement_at() {
        let retirement_at = chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .expect("valid RFC 3339 timestamp")
            .with_timezone(&chrono::Utc);
        let expected_upgrade = |retirement_at| {
            Some(ModelUpgrade {
                id: "replacement-model".to_string(),
                migration_config_key: "current-model".to_string(),
                model_link: None,
                upgrade_copy: None,
                migration_markdown: None,
                retirement_at,
            })
        };

        assert_eq!(
            vec![
                model_preset_from_api_model(api_model_with_upgrade_retirement_at(Some(
                    retirement_at.timestamp(),
                )))
                .upgrade,
                model_preset_from_api_model(api_model_with_upgrade_retirement_at(
                    /*retirement_at*/ None,
                ))
                .upgrade,
                model_preset_from_api_model(api_model_with_upgrade_retirement_at(Some(i64::MAX)))
                    .upgrade,
            ],
            vec![
                expected_upgrade(Some(retirement_at)),
                expected_upgrade(None),
                expected_upgrade(None),
            ]
        );
    }

    #[test]
    fn app_server_rate_limit_snapshots_deduplicates_top_level_limit_from_map() {
        let response = GetAccountRateLimitsResponse {
            reset_credit_confirmation: None,
            ordinary_usage_allowed: None,
            account_id: None,
            rate_limit_upsell: None,
            rate_limits: rate_limit_snapshot("codex"),
            rate_limits_by_limit_id: Some(HashMap::from([
                ("codex".to_string(), rate_limit_snapshot("codex")),
                ("other".to_string(), rate_limit_snapshot("other")),
            ])),
            rate_limit_reset_credits: None,
        };

        let snapshots = app_server_rate_limit_snapshots(response);

        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.limit_id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("codex"), Some("other")]
        );
    }

    #[test]
    fn thread_settings_update_compat_detects_unsupported_errors() {
        let cases = [
            (JSONRPC_METHOD_NOT_FOUND, "method not found", true),
            (
                JSONRPC_INVALID_REQUEST,
                "thread/settings/update requires experimentalApi capability",
                true,
            ),
            (
                JSONRPC_INVALID_REQUEST,
                "Invalid request: unknown variant `thread/settings/update`",
                true,
            ),
            (JSONRPC_INVALID_REQUEST, "invalid thread id", false),
        ];

        for (code, message, expected) in cases {
            let source = JSONRPCErrorError {
                code,
                data: None,
                message: message.to_string(),
            };
            assert_eq!(
                is_method_unsupported(&source, THREAD_SETTINGS_UPDATE_METHOD),
                expected,
                "{message}"
            );
        }
    }

    #[test]
    fn history_pagination_compat_detects_unsupported_server_fields() {
        let cases = [
            (JSONRPC_INVALID_PARAMS, "unknown field `historyMode`", true),
            (
                JSONRPC_INVALID_REQUEST,
                "thread/resume.excludeTurns requires experimentalApi capability",
                true,
            ),
            (
                JSONRPC_INVALID_REQUEST,
                "thread/fork.excludeTurns requires experimentalApi capability",
                true,
            ),
            (
                JSONRPC_METHOD_NOT_FOUND,
                "unknown method thread/turns/list",
                true,
            ),
            (JSONRPC_METHOD_NOT_FOUND, "method not found", true),
            (
                JSONRPC_INVALID_PARAMS,
                "unknown variant \"paginated\", expected \"legacy\"",
                true,
            ),
            (
                JSONRPC_INVALID_PARAMS,
                "invalid enum value `paginated`",
                true,
            ),
            (
                JSONRPC_INVALID_PARAMS,
                "paginated thread was not found",
                false,
            ),
            (JSONRPC_INVALID_PARAMS, "invalid thread id", false),
        ];

        for (code, message, expected) in cases {
            let source = JSONRPCErrorError {
                code,
                data: None,
                message: message.to_string(),
            };
            assert_eq!(
                is_history_pagination_unsupported(&source),
                expected,
                "{message}"
            );
        }
    }

    #[tokio::test]
    async fn ephemeral_thread_start_does_not_request_paginated_history() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut config = build_config(&temp_dir).await;
        config.ephemeral = true;

        let params = thread_start_params_from_config(
            &config,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            /*session_start_source*/ None,
        );

        assert_eq!(params.ephemeral, Some(true));
        assert_eq!(params.history_mode, None);
    }

    #[tokio::test]
    async fn shared_thread_start_preserves_explicit_session_overrides() -> Result<()> {
        let codex_home = tempfile::tempdir()?;
        let workspace = codex_home.path().join("workspace");
        std::fs::create_dir(&workspace)?;
        std::fs::write(
            codex_home.path().join("config.toml"),
            "sandbox_mode = \"workspace-write\"\n[sandbox_workspace_write]\nnetwork_access = true\n",
        )?;
        let server_config = build_config(&codex_home).await;
        let config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .harness_overrides(ConfigOverrides {
                cwd: Some(workspace.clone()),
                ..ConfigOverrides::default()
            })
            .cli_overrides(vec![
                (
                    "suppress_unstable_features_warning".to_string(),
                    toml::Value::Boolean(true),
                ),
                (
                    "features.multi_agent_mode".to_string(),
                    toml::Value::Boolean(true),
                ),
                (
                    "sandbox_workspace_write.network_access".to_string(),
                    toml::Value::Boolean(false),
                ),
                (
                    "instructions".to_string(),
                    toml::Value::String("unsafe ".repeat(10_000)),
                ),
                ("model".to_string(), "gpt-5".into()),
                ("approval_policy".to_string(), "never".into()),
            ])
            .build()
            .await?;

        let params = thread_start_params_from_config(
            &config,
            ThreadParamsMode::Remote,
            /*remote_cwd_override*/ None,
            /*session_start_source*/ None,
        );

        let overrides = params.config.expect("config overrides");
        assert_eq!(
            (
                overrides.get("features").cloned(),
                overrides.get("sandbox_workspace_write").cloned(),
                overrides.get("instructions").cloned(),
                overrides.get("suppress_unstable_features_warning").cloned(),
            ),
            (
                Some(serde_json::json!({ "multi_agent_mode": true })),
                Some(serde_json::json!({ "network_access": false })),
                None,
                Some(serde_json::json!(true)),
            )
        );
        for mode in [ThreadParamsMode::Embedded, ThreadParamsMode::Remote] {
            let mut app_server =
                crate::start_embedded_app_server_for_picker(&server_config).await?;
            app_server.thread_params_mode = mode;
            app_server.remote_cwd_override = Some(workspace.clone());

            let started = app_server.start_thread(&config).await?;

            assert_eq!(
                (
                    started.session.permission_profile.network_sandbox_policy(),
                    started.session.model.as_str(),
                    started.session.approval_policy,
                    started.session.cwd.to_path_buf().as_path(),
                ),
                (
                    NetworkSandboxPolicy::Restricted,
                    "gpt-5",
                    AskForApproval::Never,
                    workspace.as_path(),
                )
            );
            app_server.shutdown().await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn thread_start_params_include_cwd_for_embedded_sessions() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = ConfigBuilder::default()
            .codex_home(temp_dir.path().to_path_buf())
            .harness_overrides(ConfigOverrides {
                default_permissions: Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE.to_string()),
                ..ConfigOverrides::default()
            })
            .build()
            .await
            .expect("config should build");

        let params = thread_start_params_from_config(
            &config,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            /*session_start_source*/ None,
        );

        assert_eq!(params.cwd, Some(config.cwd.to_string_lossy().to_string()));
        assert_eq!(
            params.runtime_workspace_roots,
            Some(LegacyAppPathString::from_abs_paths(&config.workspace_roots))
        );
        assert_eq!(params.sandbox, None);
        assert_eq!(
            params.permissions,
            config
                .permissions
                .active_permission_profile()
                .map(permission_profile_id_from_active_profile)
        );
        assert_eq!(params.model_provider, None);
        assert_eq!(params.thread_source, Some(ThreadSource::User));
        assert_eq!(params.dynamic_tools, None);
    }

    #[tokio::test]
    async fn thread_start_params_can_mark_clear_source() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = build_config(&temp_dir).await;

        let params = thread_start_params_from_config(
            &config,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            Some(ThreadStartSource::Clear),
        );

        assert_eq!(params.session_start_source, Some(ThreadStartSource::Clear));
    }

    #[test]
    fn embedded_turn_permissions_use_active_profile_selection() {
        let cwd = test_path_buf("/workspace/project").abs();
        let active_permission_profile =
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE);
        let expected_permissions =
            permission_profile_id_from_active_profile(active_permission_profile.clone());

        let (sandbox_policy, permissions) = turn_permissions_overrides(
            TurnPermissionsOverride::ActiveProfile(active_permission_profile),
            cwd.as_path(),
        )
        .expect("active permission profile should be supported");

        assert_eq!(sandbox_policy, None);
        assert_eq!(permissions, Some(expected_permissions));
    }

    #[test]
    fn embedded_turn_permissions_select_profile_id_only() {
        let cwd = test_path_buf("/workspace/project").abs();
        let active_permission_profile =
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE);

        let (sandbox_policy, permissions) = turn_permissions_overrides(
            TurnPermissionsOverride::ActiveProfile(active_permission_profile),
            cwd.as_path(),
        )
        .expect("active permission profile should be supported");

        assert_eq!(sandbox_policy, None);
        assert_eq!(
            permissions,
            Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE.to_string())
        );
    }

    #[test]
    fn turn_permissions_preserve_thread_permissions_without_override() {
        let cwd = test_path_buf("/workspace/project").abs();

        let (sandbox_policy, permissions) =
            turn_permissions_overrides(TurnPermissionsOverride::Preserve, cwd.as_path())
                .expect("preserving permissions should be supported");

        assert_eq!(sandbox_policy, None);
        assert_eq!(permissions, None);
    }

    #[test]
    fn legacy_turn_permissions_project_to_sandbox_when_explicitly_overridden() {
        let cwd = test_path_buf("/workspace/project").abs();

        let (sandbox_policy, permissions) = turn_permissions_overrides(
            TurnPermissionsOverride::LegacySandbox(PermissionProfile::read_only()),
            cwd.as_path(),
        )
        .expect("read-only permission profile should be supported");

        assert_eq!(
            sandbox_policy,
            Some(codex_app_server_protocol::SandboxPolicy::ReadOnly {
                network_access: false
            })
        );
        assert_eq!(permissions, None);
    }

    #[test]
    fn legacy_turn_permissions_preserve_workspace_write() {
        let cwd = test_path_buf("/workspace/project").abs();

        let (sandbox_policy, permissions) = turn_permissions_overrides(
            TurnPermissionsOverride::LegacySandbox(PermissionProfile::workspace_write()),
            cwd.as_path(),
        )
        .expect("workspace-write permission profile should be supported");

        assert_eq!(
            sandbox_policy,
            Some(codex_app_server_protocol::SandboxPolicy::WorkspaceWrite {
                writable_roots: Vec::new(),
                network_access: false,
                exclude_tmpdir_env_var: false,
                exclude_slash_tmp: false,
            })
        );
        assert_eq!(permissions, None);
    }

    #[test]
    fn legacy_turn_permissions_reject_non_cwd_write_roots() {
        let cwd = test_path_buf("/workspace/project").abs();
        let extra_root = test_path_buf("/workspace/extra").abs();
        let permission_profile = PermissionProfile::Managed {
            network: NetworkSandboxPolicy::Restricted,
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: vec![
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::Root,
                        },
                        access: FileSystemAccessMode::Read,
                        missing_path_behavior: None,
                    },
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Path {
                            path: extra_root.into(),
                        },
                        access: FileSystemAccessMode::Write,
                        missing_path_behavior: None,
                    },
                ],
                glob_scan_max_depth: None,
            },
        };

        let error = turn_permissions_overrides(
            TurnPermissionsOverride::LegacySandbox(permission_profile),
            cwd.as_path(),
        )
        .expect_err("non-cwd write roots must not grant cwd write access");

        assert_eq!(
            error.to_string(),
            "the selected permission profile cannot be safely represented by the legacy app-server sandbox policy; select a named or legacy-compatible permission profile"
        );
    }

    #[test]
    fn legacy_turn_permissions_reject_restrictions_lost_by_projection() {
        let cwd = test_path_buf("/workspace/project").abs();
        let docs = test_path_buf("/workspace/project/docs").abs();

        for (path, access) in [
            (
                FileSystemPath::Path {
                    path: docs.clone().into(),
                },
                FileSystemAccessMode::Read,
            ),
            (
                FileSystemPath::Path { path: docs.into() },
                FileSystemAccessMode::Deny,
            ),
            (
                FileSystemPath::GlobPattern {
                    pattern: "**/*.secret".to_string(),
                },
                FileSystemAccessMode::Deny,
            ),
        ] {
            let mut permission_profile = PermissionProfile::workspace_write();
            let PermissionProfile::Managed {
                file_system: ManagedFileSystemPermissions::Restricted { entries, .. },
                ..
            } = &mut permission_profile
            else {
                unreachable!("workspace-write profiles use restricted managed permissions");
            };
            entries.push(FileSystemSandboxEntry {
                path,
                access,
                missing_path_behavior: None,
            });

            assert!(
                permission_profile
                    .to_legacy_sandbox_policy_template()
                    .is_err()
            );

            let error = turn_permissions_overrides(
                TurnPermissionsOverride::LegacySandbox(permission_profile),
                cwd.as_path(),
            )
            .expect_err("legacy projection must not discard filesystem restrictions");

            assert!(
                error
                    .downcast_ref::<UnsupportedLegacyPermissionProfile>()
                    .is_some()
            );
        }
    }

    #[test]
    fn remote_turn_permissions_preserve_active_profile_selection() {
        let cwd = test_path_buf("/workspace/project").abs();
        let active_permission_profile = ActivePermissionProfile::new("strict");
        let expected_permissions =
            permission_profile_id_from_active_profile(active_permission_profile.clone());

        let (sandbox_policy, permissions) = turn_permissions_overrides(
            TurnPermissionsOverride::ActiveProfile(active_permission_profile),
            cwd.as_path(),
        )
        .expect("active permission profile should be supported");

        assert_eq!(sandbox_policy, None);
        assert_eq!(permissions, Some(expected_permissions));
    }

    #[tokio::test]
    async fn thread_lifecycle_params_omit_local_paths_for_remote_sessions() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = build_config(&temp_dir).await;
        let thread_id = ThreadId::new();
        let expected_sandbox = sandbox_mode_from_permission_profile(
            &config.permissions.effective_permission_profile(),
            config.cwd.as_path(),
        );

        let start = thread_start_params_from_config(
            &config,
            ThreadParamsMode::Remote,
            /*remote_cwd_override*/ None,
            /*session_start_source*/ None,
        );
        let resume = thread_resume_params_from_config(
            config.clone(),
            thread_id,
            ThreadParamsMode::Remote,
            /*remote_cwd_override*/ None,
            ResumeModelSettings::OverrideFromCurrentConfig,
            crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
        );
        let fork = thread_fork_params_from_config(
            config,
            thread_id,
            ThreadParamsMode::Remote,
            /*remote_cwd_override*/ None,
        );

        assert_eq!(start.cwd, None);
        assert_eq!(resume.cwd, None);
        assert_eq!(fork.cwd, None);
        assert_eq!(start.runtime_workspace_roots, None);
        assert_eq!(resume.runtime_workspace_roots, None);
        assert_eq!(fork.runtime_workspace_roots, None);
        assert_eq!(start.model_provider, None);
        assert_eq!(resume.model_provider, None);
        assert_eq!(fork.model_provider, None);
        assert_eq!(start.sandbox, expected_sandbox);
        assert_eq!(resume.sandbox, None);
        assert_eq!(resume.approval_policy, None);
        assert_eq!(resume.approvals_reviewer, None);
        assert_eq!(fork.sandbox, expected_sandbox);
        assert_eq!(start.permissions, None);
        assert_eq!(resume.permissions, None);
        assert_eq!(fork.permissions, None);
        assert_eq!(start.thread_source, Some(ThreadSource::User));
        assert_eq!(fork.thread_source, Some(ThreadSource::User));
    }

    #[tokio::test]
    async fn remote_resume_params_keep_cwd_without_overriding_saved_permissions() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = build_config(&temp_dir).await;
        let remote_cwd = if cfg!(windows) {
            std::path::PathBuf::from("/srv/remote/project")
        } else {
            std::path::PathBuf::from(r"C:\remote\project")
        };

        let resume = thread_resume_params_from_config(
            config,
            ThreadId::new(),
            ThreadParamsMode::Remote,
            Some(remote_cwd.as_path()),
            ResumeModelSettings::RestoreFromThread,
            crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
        );

        assert_eq!(resume.cwd, Some(remote_cwd.to_string_lossy().to_string()));
        assert_eq!(resume.runtime_workspace_roots, None);
    }

    #[test]
    fn sandbox_mode_preserves_workspace_metadata_at_the_destination() {
        let workspace = tempfile::tempdir().expect("workspace");
        let cwd = workspace.path();
        std::fs::create_dir(cwd.join(".git")).expect("git metadata");
        let profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
            &codex_protocol::protocol::SandboxPolicy::new_workspace_write_policy(),
            cwd,
        );
        assert!(permission_profile_is_safely_represented_by_sandbox_mode(
            &profile, cwd
        ));
        let policy = profile
            .file_system_sandbox_policy()
            .with_additional_writable_roots(
                cwd,
                &[AbsolutePathBuf::from_absolute_path(cwd.join(".git"))
                    .expect("absolute metadata")],
            );
        let profile =
            PermissionProfile::from_runtime_permissions(&policy, NetworkSandboxPolicy::Restricted);
        assert!(!permission_profile_is_safely_represented_by_sandbox_mode(
            &profile, cwd
        ));
    }

    #[test]
    fn sandbox_mode_does_not_project_non_cwd_write_roots_for_remote_sessions() {
        let cwd = test_path_buf("/workspace/project").abs();
        let extra_root = test_path_buf("/workspace/cache").abs();
        let permission_profile: PermissionProfile = PermissionProfile::Managed {
            network: NetworkSandboxPolicy::Restricted,
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: vec![
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::Root,
                        },
                        access: FileSystemAccessMode::Read,
                        missing_path_behavior: None,
                    },
                    FileSystemSandboxEntry {
                        path: extra_root.into(),
                        access: FileSystemAccessMode::Write,
                        missing_path_behavior: None,
                    },
                ],
                glob_scan_max_depth: None,
            },
        };

        assert_eq!(
            sandbox_mode_from_permission_profile(&permission_profile, cwd.as_path()),
            Some(codex_app_server_protocol::SandboxMode::ReadOnly)
        );
        assert!(!permission_profile_is_safely_represented_by_sandbox_mode(
            &permission_profile,
            cwd.as_path(),
        ));
    }

    #[test]
    fn sandbox_mode_projects_cwd_write_for_remote_sessions() {
        let cwd = test_path_buf("/workspace/project").abs();
        let permission_profile: PermissionProfile = PermissionProfile::Managed {
            network: NetworkSandboxPolicy::Restricted,
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: vec![
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::Root,
                        },
                        access: FileSystemAccessMode::Read,
                        missing_path_behavior: None,
                    },
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::ProjectRoots { subpath: None },
                        },
                        access: FileSystemAccessMode::Write,
                        missing_path_behavior: None,
                    },
                ],
                glob_scan_max_depth: None,
            },
        };

        assert_eq!(
            sandbox_mode_from_permission_profile(&permission_profile, cwd.as_path()),
            Some(codex_app_server_protocol::SandboxMode::WorkspaceWrite)
        );
    }

    #[tokio::test]
    async fn thread_lifecycle_params_forward_explicit_remote_cwd_override_for_remote_sessions() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = build_config(&temp_dir).await;
        let thread_id = ThreadId::new();
        let remote_cwd = PathBuf::from("repo/on/server");
        let expected_sandbox = sandbox_mode_from_permission_profile(
            &config.permissions.effective_permission_profile(),
            config.cwd.as_path(),
        );

        let start = thread_start_params_from_config(
            &config,
            ThreadParamsMode::Remote,
            Some(remote_cwd.as_path()),
            /*session_start_source*/ None,
        );
        let resume = thread_resume_params_from_config(
            config.clone(),
            thread_id,
            ThreadParamsMode::Remote,
            Some(remote_cwd.as_path()),
            ResumeModelSettings::OverrideFromCurrentConfig,
            crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
        );
        let fork = thread_fork_params_from_config(
            config,
            thread_id,
            ThreadParamsMode::Remote,
            Some(remote_cwd.as_path()),
        );

        assert_eq!(start.cwd.as_deref(), Some("repo/on/server"));
        assert_eq!(resume.cwd.as_deref(), Some("repo/on/server"));
        assert_eq!(fork.cwd.as_deref(), Some("repo/on/server"));
        assert_eq!(start.runtime_workspace_roots, None);
        assert_eq!(resume.runtime_workspace_roots, None);
        assert_eq!(fork.runtime_workspace_roots, None);
        assert_eq!(start.model_provider, None);
        assert_eq!(resume.model_provider, None);
        assert_eq!(fork.model_provider, None);
        assert_eq!(start.sandbox, expected_sandbox);
        assert_eq!(resume.sandbox, None);
        assert_eq!(fork.sandbox, expected_sandbox);
        assert_eq!(start.permissions, None);
        assert_eq!(resume.permissions, None);
        assert_eq!(fork.permissions, None);
        assert_eq!(start.thread_source, Some(ThreadSource::User));
        assert_eq!(fork.thread_source, Some(ThreadSource::User));
    }

    #[tokio::test]
    async fn thread_lifecycle_params_forward_config_overrides_and_service_tier() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut config = build_config(&temp_dir).await;
        config.model_reasoning_effort = Some(ReasoningEffort::High);
        config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
        config.model_verbosity = Some(Verbosity::Low);
        config
            .web_search_mode
            .set(WebSearchMode::Disabled)
            .expect("test web search mode should be allowed");
        config.bypass_hook_trust = true;
        config.service_tier = Some(ServiceTier::Fast.request_value().to_string());
        let thread_id = ThreadId::new();

        let start = thread_start_params_from_config(
            &config,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            /*session_start_source*/ None,
        );
        let resume = thread_resume_params_from_config(
            config.clone(),
            thread_id,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            ResumeModelSettings::OverrideFromCurrentConfig,
            crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
        );
        let fork = thread_fork_params_from_config(
            config,
            thread_id,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
        );

        let expected_service_tier = Some(Some(ServiceTier::Fast.request_value().to_string()));
        assert_eq!(start.service_tier, expected_service_tier);
        assert_eq!(resume.service_tier, expected_service_tier);
        assert_eq!(fork.service_tier, expected_service_tier);
        let string = |value: &str| serde_json::Value::String(value.to_string());
        let expected_config = HashMap::from([
            ("model_reasoning_effort".to_string(), string("high")),
            ("bypass_hook_trust".to_string(), true.into()),
        ]);
        assert_eq!(start.config, Some(expected_config.clone()));
        assert_eq!(resume.config, Some(expected_config.clone()));
        assert_eq!(fork.config, Some(expected_config));
    }

    #[tokio::test]
    async fn config_overrides_forward_explicit_summary_verbosity_and_web_search() -> Result<()> {
        let home = tempfile::tempdir()?;
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".codex"))?;
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "model_reasoning_summary = \"concise\"\nmodel_verbosity = \"low\"\nweb_search = \"disabled\"\npersonality = \"none\"\n[projects.{}]\ntrust_level = \"trusted\"\n",
                toml::Value::String(workspace.to_string_lossy().into_owned()),
            ),
        )?;
        let profile = AbsolutePathBuf::from_absolute_path(home.path().join("work.config.toml"))?;
        std::fs::write(
            &profile,
            "model_reasoning_summary = \"detailed\"\nmodel_verbosity = \"high\"\nweb_search = \"cached\"\n",
        )?;
        for (selected_profile, project, cli, expected) in [
            (false, false, false, [None, None, None]),
            (
                true,
                false,
                false,
                [Some("detailed"), Some("high"), Some("cached")],
            ),
            (
                false,
                false,
                true,
                [Some("auto"), Some("medium"), Some("live")],
            ),
            (
                true,
                false,
                true,
                [Some("auto"), Some("medium"), Some("live")],
            ),
            (true, true, false, [None, None, None]),
            (
                true,
                true,
                true,
                [Some("auto"), Some("medium"), Some("live")],
            ),
        ] {
            std::fs::write(
                workspace.join(".codex/config.toml"),
                if project {
                    "model_reasoning_summary = \"concise\"\nmodel_verbosity = \"low\"\nweb_search = \"disabled\"\n"
                } else {
                    ""
                },
            )?;
            let config = ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .harness_overrides(ConfigOverrides {
                    cwd: Some(workspace.clone()),
                    ..ConfigOverrides::default()
                })
                .loader_overrides(codex_config::LoaderOverrides {
                    user_config_path: selected_profile.then(|| profile.clone()),
                    user_config_profile: selected_profile.then(|| "work".parse().unwrap()),
                    ..codex_config::LoaderOverrides::without_managed_config_for_tests()
                })
                .cli_overrides(if cli {
                    vec![
                        ("model_reasoning_summary".to_string(), "auto".into()),
                        ("model_verbosity".to_string(), "medium".into()),
                        ("web_search".to_string(), "live".into()),
                        ("personality".to_string(), "none".into()),
                    ]
                } else {
                    Vec::new()
                })
                .build()
                .await?;
            let overrides = config_request_overrides_from_config(&config, ThreadParamsMode::Remote)
                .expect("config overrides");
            assert_eq!(
                (
                    ["model_reasoning_summary", "model_verbosity", "web_search"]
                        .map(|key| overrides.get(key).and_then(serde_json::Value::as_str)),
                    overrides
                        .get("personality")
                        .and_then(serde_json::Value::as_str),
                ),
                (expected, cli.then_some("none")),
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn thread_resume_params_can_restore_persisted_model_settings() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut config = build_config(&temp_dir).await;
        config.model = Some("configured-model".to_string());
        config.model_provider_id = "configured-provider".to_string();
        config.model_reasoning_effort = Some(ReasoningEffort::Ultra);
        config.model_reasoning_summary = Some(ReasoningSummary::Detailed);

        let params = thread_resume_params_from_config(
            config,
            ThreadId::new(),
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            ResumeModelSettings::RestoreFromThread,
            crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
        );

        assert_eq!(params.model, None);
        assert_eq!(params.model_provider, None);
        assert_eq!(params.config, None);
    }

    #[tokio::test]
    async fn thread_resume_params_can_rejoin_without_overriding_existing_settings() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = build_config(&temp_dir).await;
        let thread_id = ThreadId::new();

        let params = thread_resume_params_from_config(
            config,
            thread_id,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            ResumeModelSettings::PreserveExistingThread,
            crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
        );

        assert_eq!(
            params,
            ThreadResumeParams {
                thread_id: thread_id.to_string(),
                ..ThreadResumeParams::default()
            }
        );
    }

    #[tokio::test]
    async fn connected_thread_start_preserves_flex_without_catalog_support() -> Result<()> {
        for fast_mode_enabled in [false, true] {
            let codex_home = tempfile::tempdir()?;
            let server_config = build_config(&codex_home).await;
            let mut config = server_config.clone();
            config.model = Some("gpt-5.5".to_string());
            config.service_tier = Some(ServiceTier::Flex.request_value().to_string());
            config
                .features
                .set_enabled(Feature::FastMode, fast_mode_enabled)?;
            let mut app_server =
                crate::start_embedded_app_server_for_picker(&server_config).await?;
            app_server.thread_params_mode = ThreadParamsMode::Remote;
            let mut preset = crate::test_support::TEST_MODEL_PRESETS
                .iter()
                .find(|preset| preset.model == "gpt-5.5")
                .expect("gpt-5.5 test preset")
                .clone();
            preset.service_tiers.clear();
            preset.default_service_tier = None;
            app_server.available_models = vec![preset];

            let started = app_server.start_thread(&config).await?;

            assert_eq!(
                started.session.service_tier,
                Some(ServiceTier::Flex.request_value().to_string())
            );
            app_server.shutdown().await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn persisted_resume_does_not_forward_implicit_service_tier() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let mut config = build_config(&codex_home).await;
        config.model = Some("gpt-5.5".to_string());
        config.service_tier = None;
        config
            .features
            .enable(Feature::FastMode)
            .expect("enable fast mode");
        let thread_id = ThreadId::from_string(
            &create_fake_rollout(
                codex_home.path(),
                "2025-01-05T12-00-00",
                "2025-01-05T12:00:00Z",
                "Saved user message",
                Some(config.model_provider_id.as_str()),
                /*git_info*/ None,
            )
            .expect("create source rollout"),
        )?;
        let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
        let mut preset = crate::test_support::TEST_MODEL_PRESETS
            .iter()
            .find(|preset| preset.model == "gpt-5.5")
            .expect("gpt-5.5 test preset")
            .clone();
        preset.service_tiers = vec![ModelServiceTier {
            id: ServiceTier::Fast.request_value().to_string(),
            name: "fast".to_string(),
            description: "Fast tier".to_string(),
        }];
        preset.default_service_tier = Some(ServiceTier::Fast.request_value().to_string());
        app_server.available_models = vec![preset];

        let resumed = app_server
            .resume_thread(
                &LocalSettings::from(&config),
                config,
                thread_id,
                ResumeModelSettings::RestoreFromThread,
            )
            .await?;

        assert_eq!(resumed.session.service_tier, None);
        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn side_fork_skips_parent_title_lookup_but_normal_ephemeral_fork_keeps_it() -> Result<()>
    {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let config = build_config(&codex_home).await;
        let source_thread_id = ThreadId::from_string(
            &create_fake_rollout(
                codex_home.path(),
                "2025-01-05T12-00-00",
                "2025-01-05T12:00:00Z",
                "Saved user message",
                Some(config.model_provider_id.as_str()),
                /*git_info*/ None,
            )
            .expect("create source rollout"),
        )?;
        let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
        app_server
            .resume_thread(
                &LocalSettings::from(&config),
                config.clone(),
                source_thread_id,
                ResumeModelSettings::RestoreFromThread,
            )
            .await?;
        app_server
            .thread_set_name(source_thread_id, "Source thread".to_string())
            .await?;

        let mut ephemeral_config = config;
        ephemeral_config.ephemeral = true;
        let normal_ephemeral_fork = app_server
            .fork_thread(
                &LocalSettings::from(&ephemeral_config),
                ephemeral_config.clone(),
                source_thread_id,
            )
            .await?;
        let side_fork = app_server
            .fork_side_thread(
                &LocalSettings::from(&ephemeral_config),
                ephemeral_config,
                /*workspace_roots*/ None,
                source_thread_id,
                /*selected_profile*/ None,
                crate::app_server_session::SideChatStorage::Temporary,
            )
            .await?;

        assert_eq!(
            normal_ephemeral_fork.session.fork_parent_title.as_deref(),
            Some("Source thread")
        );
        assert_eq!(side_fork.session.fork_parent_title, None);
        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn ephemeral_paginated_fork_skips_unsupported_history_hydration() -> Result<()> {
        let codex_home = tempfile::tempdir()?;
        let config = build_config(&codex_home).await;
        let source_thread_id = ThreadId::from_string(
            &create_fake_paginated_rollout(
                codex_home.path(),
                "2025-01-05T12-00-00",
                "2025-01-05T12:00:00Z",
                "Saved user message",
                Some(config.model_provider_id.as_str()),
                /*git_info*/ None,
            )
            .map_err(|error| {
                color_eyre::eyre::eyre!("failed to create paginated rollout: {error}")
            })?,
        )?;
        let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
        let mut ephemeral_config = config;
        ephemeral_config.ephemeral = true;

        let fork = app_server
            .fork_thread(
                &LocalSettings::from(&ephemeral_config),
                ephemeral_config,
                source_thread_id,
            )
            .await?;

        assert_eq!(fork.session.forked_from_id, Some(source_thread_id));
        assert!(fork.turns.is_empty());
        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn side_fork_uses_one_request_for_long_paginated_history() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let mut config = build_config(&codex_home).await;
        config.terminal_resize_reflow.max_rows =
            crate::legacy_core::config::TerminalResizeReflowMaxRows::Limit(100);
        let filename_ts = "2025-01-05T12-00-00";
        let source_id = create_fake_paginated_rollout(
            codex_home.path(),
            filename_ts,
            "2025-01-05T12:00:00Z",
            "Saved user message",
            Some(config.model_provider_id.as_str()),
            /*git_info*/ None,
        )
        .expect("create long paginated source rollout");
        let source_path =
            app_test_support::rollout_path(codex_home.path(), filename_ts, source_id.as_str());
        let mut contents = std::fs::read_to_string(&source_path)?;
        let rollout_line = |ordinal: usize, payload: serde_json::Value| {
            serde_json::json!({
                "timestamp": "2025-01-05T12:00:00Z",
                "type": "event_msg",
                "payload": payload,
                "ordinal": ordinal,
            })
        };
        let started = rollout_line(
            /*ordinal*/ 3,
            serde_json::json!({
                "type": "task_started",
                "turn_id": "long-history-turn",
                "model_context_window": null,
            }),
        );
        contents.push_str(&format!("{started}\n"));
        for index in 0..256 {
            let item = rollout_line(
                index + 4,
                serde_json::json!({
                    "type": "item_completed",
                    "thread_id": source_id,
                    "turn_id": "long-history-turn",
                    "item": {
                        "type": "UserMessage",
                        "id": format!("long-history-user-{index}"),
                        "content": [{
                            "type": "text",
                            "text": format!("long history message {index}"),
                        }],
                    },
                }),
            );
            contents.push_str(&format!("{item}\n"));
        }
        std::fs::write(source_path, contents)?;

        let source_thread_id = ThreadId::from_string(source_id.as_str())?;
        let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;
        let resumed = app_server
            .resume_thread(
                &LocalSettings::from(&config),
                config.clone(),
                source_thread_id,
                ResumeModelSettings::RestoreFromThread,
            )
            .await?;
        let loaded_items: usize = resumed.turns.iter().map(|turn| turn.items.len()).sum();
        assert!(loaded_items <= HISTORY_ITEM_PAGE_LIMIT as usize);
        assert!(app_server.has_older_history(source_thread_id));

        let mut side_config = config;
        side_config.ephemeral = true;
        let side = app_server
            .fork_side_thread(
                &LocalSettings::from(&side_config),
                side_config,
                /*workspace_roots*/ None,
                source_thread_id,
                /*selected_profile*/ None,
                crate::app_server_session::SideChatStorage::Temporary,
            )
            .await?;

        assert_eq!(side.session.forked_from_id, Some(source_thread_id));
        assert_eq!(side.turns, Vec::<Turn>::new());
        assert!(app_server.has_older_history(source_thread_id));
        assert!(
            !app_server
                .history_pagination
                .contains_key(&side.session.thread_id)
        );

        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn thread_fork_params_forward_instruction_overrides() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut config = build_config(&temp_dir).await;
        config.base_instructions = Some("Base override.".to_string());
        config.base_instructions_provenance = Some(BaseInstructionsProvenance::Custom);
        config.developer_instructions = Some("Developer override.".to_string());
        let thread_id = ThreadId::new();

        let params = thread_fork_params_from_config(
            config.clone(),
            thread_id,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
        );

        assert_eq!(params.base_instructions.as_deref(), Some("Base override."));
        assert_eq!(
            params.developer_instructions.as_deref(),
            Some("Developer override.")
        );

        config.base_instructions_provenance = Some(BaseInstructionsProvenance::Model {
            model: "gpt-5.2".to_string(),
        });
        let params = thread_fork_params_from_config(
            config,
            thread_id,
            ThreadParamsMode::Remote,
            /*remote_cwd_override*/ None,
        );

        assert_eq!(params.base_instructions, None);
    }

    #[tokio::test]
    async fn side_fork_excludes_turns_without_clearing_regular_ephemeral_fork() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let mut config = build_config(&codex_home).await;
        config.ephemeral = true;
        let thread_id = ThreadId::from_string(
            &create_fake_rollout(
                codex_home.path(),
                "2025-01-05T12-00-00",
                "2025-01-05T12:00:00Z",
                "Saved user message",
                Some(config.model_provider_id.as_str()),
                /*git_info*/ None,
            )
            .expect("create rollout"),
        )?;
        let mut app_server = crate::start_embedded_app_server_for_picker(&config).await?;

        let regular = app_server
            .fork_thread(&LocalSettings::from(&config), config.clone(), thread_id)
            .await?;
        let side = app_server
            .fork_side_thread(
                &LocalSettings::from(&config),
                config,
                /*workspace_roots*/ None,
                thread_id,
                /*selected_profile*/ None,
                crate::app_server_session::SideChatStorage::Temporary,
            )
            .await?;

        assert_eq!(regular.turns.len(), 1);
        assert!(matches!(
            regular.turns[0].items.as_slice(),
            [codex_app_server_protocol::ThreadItem { payload: codex_app_server_protocol::ThreadItemPayload::UserMessage { content, .. }, .. }]
                if content == &[UserInput::Text {
                    text: "Saved user message".to_string(),
                    text_elements: Vec::new(),
                }]
        ));
        assert_eq!(side.turns, Vec::<Turn>::new());
        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn terminal_visualization_instructions_are_gated_for_all_tui_thread_flows() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut config = build_config(&temp_dir).await;
        config.developer_instructions = Some("Developer override.".to_string());
        let thread_id = ThreadId::new();

        let control_start = thread_start_params_from_config(
            &config,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            /*session_start_source*/ None,
        );
        let control_resume = thread_resume_params_from_config(
            config.clone(),
            thread_id,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            ResumeModelSettings::OverrideFromCurrentConfig,
            crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
        );
        let control_fork = thread_fork_params_from_config(
            config.clone(),
            thread_id,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
        );

        assert_eq!(control_start.developer_instructions, None);
        assert_eq!(control_resume.developer_instructions, None);
        assert_eq!(
            control_fork.developer_instructions.as_deref(),
            Some("Developer override.")
        );

        let _ = config
            .features
            .enable(Feature::TerminalVisualizationInstructions);
        let treatment_start = thread_start_params_from_config(
            &config,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            /*session_start_source*/ None,
        );
        let treatment_resume = thread_resume_params_from_config(
            config.clone(),
            thread_id,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
            ResumeModelSettings::OverrideFromCurrentConfig,
            crate::resume_permissions::ResumePermissions::CURRENT_CONFIG,
        );
        let treatment_fork = thread_fork_params_from_config(
            config,
            thread_id,
            ThreadParamsMode::Embedded,
            /*remote_cwd_override*/ None,
        );
        let expected = format!(
            "Developer override.\n\n{}",
            crate::terminal_visualization_instructions::TERMINAL_VISUALIZATION_INSTRUCTIONS
        );

        assert_eq!(
            treatment_start.developer_instructions.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            treatment_resume.developer_instructions.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            treatment_fork.developer_instructions.as_deref(),
            Some(expected.as_str())
        );
    }

    #[tokio::test]
    async fn resume_response_restores_turns_from_thread_items() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = build_config(&temp_dir).await;
        let thread_id = ThreadId::new();
        let forked_from_id = ThreadId::new();
        let read_only_profile = PermissionProfile::read_only();
        let response = ThreadResumeResponse {
            disabled_plugin_ids: Vec::new(),
            thread: codex_app_server_protocol::Thread {
                theme: None,
                originator: None,
                environments: Some(vec![codex_app_server_protocol::ThreadEnvironment {
                    environment_id: "remote".into(),
                    cwd: LegacyAppPathString::from_string("/executor/work"),
                    runtime_workspace_roots: Vec::new(),
                }]),
                id: thread_id.to_string(),
                extra: None,
                session_id: ThreadId::new().to_string(),
                forked_from_id: Some(forked_from_id.to_string()),
                attached_to_thread_id: None,
                parent_thread_id: None,
                preview: "hello".to_string(),
                ephemeral: false,
                section: None,
                section_entered_at: None,
                project_id: None,
                daybreak_enabled: None,
                comms: Default::default(),
                context_role: None,
                daybreak_program_propagation_enrolled: None,
                history_mode: Default::default(),
                model_provider: "openai".to_string(),
                model: None,
                reasoning_effort: None,
                created_at: 1,
                updated_at: 2,
                recency_at: Some(2),
                status: ThreadStatus::Idle,
                path: None,
                cwd: test_path_buf("/tmp/project").abs().into(),
                cli_version: "0.0.0".to_string(),
                source: codex_app_server_protocol::SessionSource::Cli,
                can_accept_text_input: None,
                can_accept_direct_input: None,
                thread_source: None,
                agent_nickname: None,
                agent_role: None,
                git_info: None,
                name: None,
                turns: vec![Turn {
                    id: "turn-1".to_string(),
                    root_turn_id: None,
                    items_view: codex_app_server_protocol::TurnItemsView::Full,
                    items: vec![
                        ThreadItem::untimed(
                            codex_app_server_protocol::ThreadItemPayload::UserMessage {
                                source_response_item_id: None,
                                id: "user-1".to_string(),
                                client_id: None,
                                content: vec![codex_app_server_protocol::UserInput::Text {
                                    text: "hello from history".to_string(),
                                    text_elements: Vec::new(),
                                }],
                            },
                        ),
                        ThreadItem::untimed(
                            codex_app_server_protocol::ThreadItemPayload::AgentMessage {
                                moderation: None,
                                id: "assistant-1".to_string(),
                                text: "assistant reply".to_string(),
                                phase: None,
                                memory_citation: None,
                                delivery: None,
                                questions: None,
                            },
                        ),
                    ],
                    status: TurnStatus::Completed,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                }],
            },
            model: "gpt-5.5".to_string(),
            model_provider: "openai".to_string(),
            service_tier: None,
            cwd: test_path_buf("/tmp/project").abs().into(),
            runtime_workspace_roots: vec![
                test_path_buf("/tmp/project").abs().into(),
                test_path_buf("/tmp/project/extra").abs().into(),
            ],
            instruction_sources: vec![LegacyAppPathString::from_abs_path(
                &test_path_buf("/tmp/project/AGENTS.md").abs(),
            )],
            approval_policy: codex_app_server_protocol::AskForApproval::Never,
            approvals_reviewer: codex_app_server_protocol::ApprovalsReviewer::User,
            sandbox: read_only_profile
                .to_legacy_sandbox_policy(test_path_buf("/tmp/project").as_path())
                .expect("read-only profile must be legacy-compatible")
                .into(),
            active_permission_profile: None,
            reasoning_effort: None,
            collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
                mode: codex_protocol::config_types::ModeKind::Plan,
                settings: codex_protocol::config_types::Settings {
                    model: "gpt-5.5".to_string(),
                    reasoning_effort: None,
                    developer_instructions: Some("Keep planning".to_string()),
                },
            }),
            multi_agent_mode: Default::default(),
            initial_turns_page: None,
            turns_backwards_cursor: None,
            items_backwards_cursor: None,
        };

        let started = started_thread_from_resume_response(
            response.clone(),
            &LocalSettings::from(&config),
            /*cwd_convention*/ None,
        )
        .await
        .expect("resume response should map");
        assert_eq!(started.session.forked_from_id, Some(forked_from_id));
        assert_eq!(
            started.session.runtime_workspace_roots,
            response
                .runtime_workspace_roots
                .clone()
                .into_iter()
                .map(PathUri::try_from)
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        );
        assert_eq!(
            started.session.instruction_source_paths,
            response.instruction_source_path_uris()
        );
        assert_eq!(started.session.permission_profile, read_only_profile);
        assert_eq!(started.turns.len(), 1);
        assert_eq!(started.turns[0], response.thread.turns[0]);
        assert!(!started.blocks_direct_input);

        // The first prompt after resume must preserve the server's restored mode.
        let (mut chat, _sender, _events, mut commands) =
            crate::chatwidget::tests::helpers::make_chatwidget_manual_with_sender().await;
        chat.handle_thread_session(started.session);
        insta::assert_snapshot!(
            "resumed_plan_mode",
            crate::chatwidget::tests::helpers::normalize_snapshot_paths(
                crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 40),
            )
        );
        chat.handle_paste("Continue planning".to_string());
        chat.handle_key_event(crossterm::event::KeyCode::Enter.into());
        let submitted_mode = std::iter::from_fn(|| commands.try_recv().ok()).find_map(|command| {
            if let crate::app_command::AppCommand::UserTurn {
                collaboration_mode, ..
            } = command
            {
                Some(collaboration_mode)
            } else {
                None
            }
        });
        assert_eq!(submitted_mode, Some(response.collaboration_mode.clone()));

        let embedded_config = ConfigBuilder::default()
            .codex_home(temp_dir.path().join("embedded-codex-home"))
            .harness_overrides(ConfigOverrides {
                default_permissions: Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE.to_string()),
                ..ConfigOverrides::default()
            })
            .build()
            .await
            .expect("config should build");
        let started = started_thread_from_resume_response(
            response.clone(),
            &LocalSettings::from(&embedded_config),
            /*cwd_convention*/ None,
        )
        .await
        .expect("embedded resume response should map");
        assert_eq!(started.session.permission_profile, read_only_profile);

        let mut empty_roots_response = response;
        empty_roots_response.runtime_workspace_roots = Vec::new();
        let server_cwd = r"/\repo";
        empty_roots_response.cwd = LegacyAppPathString::from_string(server_cwd);
        let started = started_thread_from_resume_response(
            empty_roots_response,
            &LocalSettings::from(&config),
            Some(PathConvention::Posix),
        )
        .await
        .expect("resume response should map");
        assert_eq!(started.session.runtime_workspace_roots, Vec::new());
    }

    #[tokio::test]
    async fn remote_thread_response_uses_legacy_sandbox_fallback() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = build_config(&temp_dir).await;
        let cwd = test_path_buf("/tmp/project").abs();
        let sandbox = PermissionProfile::read_only()
            .to_legacy_sandbox_policy(cwd.as_path())
            .expect("read-only profile must be legacy-compatible")
            .into();

        assert_eq!(
            display_permission_profile_from_thread_response(
                &sandbox,
                &PathUri::from_abs_path(&cwd),
                &config,
                ThreadParamsMode::Remote,
            )
            .unwrap(),
            PermissionProfile::read_only()
        );
    }

    #[tokio::test]
    async fn embedded_thread_response_uses_local_config_profile() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = ConfigBuilder::default()
            .codex_home(temp_dir.path().to_path_buf())
            .harness_overrides(ConfigOverrides {
                default_permissions: Some(BUILT_IN_PERMISSION_PROFILE_READ_ONLY.to_string()),
                ..ConfigOverrides::default()
            })
            .build()
            .await
            .expect("config should build");
        let cwd = test_path_buf("/tmp/project").abs();

        assert_eq!(
            display_permission_profile_from_thread_response(
                &codex_app_server_protocol::SandboxPolicy::DangerFullAccess,
                &PathUri::from_abs_path(&cwd),
                &config,
                ThreadParamsMode::Embedded,
            )
            .unwrap(),
            PermissionProfile::read_only()
        );
    }

    #[tokio::test]
    async fn session_configured_populates_history_metadata() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = build_config(&temp_dir).await;
        let thread_id = ThreadId::new();

        let history_config =
            codex_message_history::HistoryConfig::new(config.codex_home.clone(), &config.history);

        codex_message_history::append_entry("older", &thread_id, &history_config)
            .await
            .expect("history append should succeed");
        codex_message_history::append_entry("newer", &thread_id, &history_config)
            .await
            .expect("history append should succeed");

        let session = thread_session_state_from_thread_response(
            &thread_id.to_string(),
            /*environments*/ None,
            /*forked_from_id*/ None,
            Some("restore".to_string()),
            /*rollout_path*/ None,
            "gpt-5.4".to_string(),
            "openai".to_string(),
            /*service_tier*/ None,
            AskForApproval::Never,
            codex_protocol::config_types::ApprovalsReviewer::User,
            PermissionProfile::read_only(),
            /*active_permission_profile*/ None,
            test_path_buf("/tmp/project").abs().into(),
            Vec::new(),
            Vec::new(),
            /*reasoning_effort*/ None,
            &LocalSettings::from(&config),
        )
        .await
        .expect("session should map");

        let metadata = session
            .message_history
            .expect("session should include message-history metadata");
        assert_ne!(metadata.log_id, 0);
        assert_eq!(metadata.entry_count, 2);
    }

    #[tokio::test]
    async fn session_configured_preserves_fork_source_thread_id() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config = build_config(&temp_dir).await;
        let thread_id = ThreadId::new();
        let forked_from_id = ThreadId::new();

        let session = thread_session_state_from_thread_response(
            &thread_id.to_string(),
            /*environments*/ None,
            Some(forked_from_id.to_string()),
            Some("restore".to_string()),
            /*rollout_path*/ None,
            "gpt-5.4".to_string(),
            "openai".to_string(),
            /*service_tier*/ None,
            AskForApproval::Never,
            codex_protocol::config_types::ApprovalsReviewer::User,
            PermissionProfile::read_only(),
            /*active_permission_profile*/ None,
            test_path_buf("/tmp/project").abs().into(),
            Vec::new(),
            Vec::new(),
            /*reasoning_effort*/ None,
            &LocalSettings::from(&config),
        )
        .await
        .expect("session should map");

        assert_eq!(session.forked_from_id, Some(forked_from_id));
    }

    #[test]
    fn session_account_uses_remapped_plan_labels() {
        for (plan, label) in [
            (
                codex_protocol::account::PlanType::EnterpriseCbpUsageBased,
                "Enterprise",
            ),
            (
                codex_protocol::account::PlanType::SelfServeBusinessUsageBased,
                "Business",
            ),
            (
                codex_protocol::account::PlanType::SelfServeBusinessProLite,
                "Business Premium",
            ),
        ] {
            let account = SessionAccount::from(GetAccountResponse {
                account_id: None,
                account: Some(codex_app_server_protocol::Account::Chatgpt {
                    email: None,
                    plan_type: plan,
                    workspace_name: None,
                }),
                display_name: None,
                user_id: None,
                requires_openai_auth: true,
                workspace_routing: None,
            });
            assert!(
                matches!(account.display, Some(StatusAccountDisplay::ChatGpt { email: None, plan: Some(ref name), .. }) if name == label)
            );
        }
    }
}
