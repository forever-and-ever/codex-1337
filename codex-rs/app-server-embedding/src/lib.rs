//! The contract between a process that embeds an app-server and the runtime serving it.
//!
//! Clients start an in-process app-server through this contract instead of depending on
//! the server crate, so editing the server leaves them compiled. Every binary that embeds
//! a server calls `codex_app_server::install_embedded_runtime` once before starting one.

mod network_policy;

use std::io::Error as IoError;
use std::io::Result as IoResult;
use std::sync::Arc;
use std::sync::OnceLock;

use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientNotification;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConfigWarningNotification;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::InitializeParams;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::Result as JsonRpcResult;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_arg0::Arg0DispatchPaths;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_core::config::Config;
use codex_exec_server::EnvironmentManager;
use codex_feedback::CodexFeedback;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::SessionSource;
use codex_utils_path_uri::PathUri;
use futures::future::BoxFuture;
use toml::Value as TomlValue;

pub use codex_rollout::StateDbHandle;
pub use codex_state::log_db::LogDbLayer;
pub use network_policy::EmbeddedNetworkPolicy;
pub use network_policy::destination_policy;

/// Default bounded channel capacity for in-process runtime queues.
pub const DEFAULT_IN_PROCESS_CHANNEL_CAPACITY: usize = codex_app_server_transport::CHANNEL_CAPACITY;

/// Raw app-server request result for typed in-process requests.
///
/// Even on the in-process path, successful responses still travel back through
/// the same JSON-RPC result envelope used by socket/stdio transports because
/// `MessageProcessor` continues to produce that shape internally.
pub type RequestResult = Result<JsonRpcResult, JSONRPCErrorError>;

/// Event emitted from the app-server to the in-process client.
///
/// [`Lagged`](Self::Lagged) is a transport health marker, not an application
/// event — it signals that the consumer fell behind and some events were dropped.
#[derive(Debug, Clone)]
pub enum InProcessServerEvent {
    /// Server request that requires client response/rejection.
    ServerRequest(Box<ServerRequest>),
    /// App-server notification directed to the embedded client.
    ServerNotification(Box<ServerNotification>),
    /// Indicates one or more events were dropped due to backpressure.
    Lagged { skipped: usize },
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent startup policy and advertised protocol capabilities are orthogonal, not exclusive states"
)]
#[derive(Clone)]
pub struct InProcessClientStartArgs {
    /// This client collects fresh human decisions for managed command checkpoints.
    pub required_human_approval: bool,
    /// Resolved argv0 dispatch paths used by command execution internals.
    pub arg0_paths: Arg0DispatchPaths,
    /// Shared config used to initialize app-server runtime.
    pub config: Arc<Config>,
    /// CLI config overrides that are already parsed into TOML values.
    pub cli_overrides: Vec<(String, TomlValue)>,
    /// Loader override knobs used by config API paths.
    pub loader_overrides: LoaderOverrides,
    /// Whether config API paths should reject unknown config fields.
    pub strict_config: bool,
    /// Preloaded cloud config bundle provider.
    pub cloud_config_bundle: CloudConfigBundleLoader,
    /// Policy shared with transports created by the embedder before startup.
    pub embedded_network_policy: EmbeddedNetworkPolicy,
    /// Feedback sink used by app-server/core telemetry and logs.
    pub feedback: CodexFeedback,
    /// `SQLite` tracing layer used to flush recently emitted logs before feedback upload.
    pub log_db: Option<LogDbLayer>,
    /// Process-wide `SQLite` state handle shared with the embedded app-server.
    pub state_db: Option<StateDbHandle>,
    /// Environment manager used by core execution and filesystem operations.
    pub environment_manager: Arc<EnvironmentManager>,
    /// Startup warnings emitted after initialize succeeds.
    pub config_warnings: Vec<ConfigWarningNotification>,
    /// Session source recorded in app-server thread metadata.
    pub session_source: SessionSource,
    /// Whether auth loading should honor the `CODEX_API_KEY` environment variable.
    pub enable_codex_api_key_env: bool,
    /// Client name reported during initialize.
    pub client_name: String,
    /// Client version reported during initialize.
    pub client_version: String,
    /// Whether experimental APIs are requested at initialize time.
    pub experimental_api: bool,
    /// Whether MCP servers may send `openai/form` elicitation requests.
    pub mcp_server_openai_form_elicitation: bool,
    /// Notification methods this client opts out of receiving.
    pub opt_out_notification_methods: Vec<String>,
    /// Queue capacity for command and embedded-runtime channels (clamped to at least 1).
    pub channel_capacity: usize,
}

impl InProcessClientStartArgs {
    /// Builds initialize params from caller-provided metadata.
    pub fn initialize_params(&self) -> InitializeParams {
        let capabilities = InitializeCapabilities {
            required_human_approval: self.required_human_approval,
            explicit_gateway_oauth: false,
            experimental_api: self.experimental_api,
            request_attestation: false,
            extensions: None,
            opt_out_notification_methods: if self.opt_out_notification_methods.is_empty() {
                None
            } else {
                Some(self.opt_out_notification_methods.clone())
            },
            mcp_server_openai_form_elicitation: self.mcp_server_openai_form_elicitation,
        };

        InitializeParams {
            client_info: ClientInfo {
                name: self.client_name.clone(),
                title: None,
                version: self.client_version.clone(),
            },
            capabilities: Some(capabilities),
        }
    }
}

/// Effective permissions and roots for a loaded embedded thread.
pub struct ThreadPermissions {
    pub profile: PermissionProfile,
    pub workspace_roots: Vec<PathUri>,
}

/// Read-only access to loaded thread permissions in the embedded runtime.
pub type ThreadPermissionsReader =
    Arc<dyn Fn(ThreadId) -> BoxFuture<'static, IoResult<ThreadPermissions>> + Send + Sync>;

/// The server half of one in-process connection.
pub trait EmbeddedServer: Send {
    /// Returns a sender that remains usable while the server runs.
    fn connection(&self) -> Arc<dyn EmbeddedConnection>;

    fn thread_permissions_reader(&self) -> ThreadPermissionsReader;

    /// Yields the next server event, or `None` once the server has stopped.
    fn next_event(&mut self) -> BoxFuture<'_, Option<InProcessServerEvent>>;

    /// Drains the server and waits for its runtime to finish.
    fn shutdown(self: Box<Self>) -> BoxFuture<'static, IoResult<()>>;
}

/// Client-to-server traffic of one in-process connection.
pub trait EmbeddedConnection: Send + Sync {
    /// Sends a request; dropping the returned future cancels it.
    fn request(&self, request: ClientRequest) -> BoxFuture<'static, IoResult<RequestResult>>;

    fn notify(&self, notification: ClientNotification) -> IoResult<()>;

    fn respond_to_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> IoResult<()>;

    fn fail_server_request(&self, request_id: RequestId, error: JSONRPCErrorError) -> IoResult<()>;
}

/// Starts an app-server inside this process.
pub type EmbeddedStart =
    fn(InProcessClientStartArgs) -> BoxFuture<'static, IoResult<Box<dyn EmbeddedServer>>>;

static RUNTIME: OnceLock<EmbeddedStart> = OnceLock::new();

/// Installs this process's app-server runtime; the first installation stands.
pub fn install(start: EmbeddedStart) {
    RUNTIME.get_or_init(|| start);
}

/// Starts the installed app-server runtime.
pub async fn start(args: InProcessClientStartArgs) -> IoResult<Box<dyn EmbeddedServer>> {
    let start = RUNTIME
        .get()
        .ok_or_else(|| IoError::other("this process installed no embedded app-server runtime"))?;
    start(args).await
}
