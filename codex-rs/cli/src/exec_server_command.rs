//! Command-line startup for `codex exec-server`.
//!
//! Transport, configuration, authentication, and shutdown are kept together.

use anyhow::Context;
use codex_cloud_config::managed_config_bundle_loader_for_auth;
use std::io::BufRead;
use std::io::Read;
use std::sync::Arc;

use clap::Parser;
use codex_app_server_embedding::EmbeddedNetworkPolicy;
use codex_arg0::Arg0DispatchPaths;
use codex_cloud_config::chatgpt_config_bundle_loader;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core::config::ConfigLoadOptions;
use codex_core::config::bootstrap_auth_config;
use codex_core::config::find_codex_home;
use codex_core::config::load_config_toml_with_layer_stack;
use codex_exec_server::ExecServerRuntimeOptions;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::read_codex_access_token_from_env;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_cli::CliConfigOverrides;
use codex_websocket_auth::WebsocketAuthArgs;

#[path = "exec_server_auth.rs"]
mod registry_auth;

use crate::exec_server_credential::ExecutorCredentialPipe;
use crate::exec_server_telemetry;
use crate::exec_server_telemetry::ParentLifetime;

/// [EXPERIMENTAL] Run the standalone exec-server service.
#[derive(Debug, Parser)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "clap boundary mirroring upstream's exec-server flags; clap enforces the conflicts; parsed into a typed choice at the boundary"
)]
pub(super) struct ExecServerCommand {
    #[command(subcommand)]
    pub(super) command: Option<ExecServerSubcommand>,

    /// Error out when config.toml contains fields that are not recognized by this version of Codex.
    #[arg(
        id = "exec_server_strict_config",
        long = "strict-config",
        default_value_t = false,
        global = true
    )]
    pub(super) strict_config: bool,

    /// Linux PID namespace: isolate (default) or inherit. Inherit allows signals to
    /// other same-UID processes; enable only when provisioning a dedicated environment.
    #[arg(long, value_name = "MODE", default_value = "isolate", global = true)]
    linux_sandbox_pid_namespace: codex_sandboxing::LinuxSandboxPidNamespace,

    /// Allow permitted private IP destinations to use the configured upstream proxy.
    /// If no valid upstream proxy applies to the request protocol, connect directly.
    /// Loopback stays local. This flag does not require upstream routing.
    #[arg(
        long,
        env = "CODEX_EXEC_SERVER_PROXY_PRIVATE_IPS_VIA_UPSTREAM",
        global = true
    )]
    proxy_private_ips_via_upstream: bool,

    /// Maximum number of requests to process concurrently on each connection.
    #[arg(
        long = "concurrent-requests",
        value_name = "COUNT",
        default_value = "1"
    )]
    request_dispatch_mode: codex_exec_server::RequestDispatchMode,

    /// Transport endpoint URL. Supported values: `ws://IP:PORT` (default), `stdio`, `stdio://`.
    #[arg(
        long = "listen",
        value_name = "URL",
        conflicts_with = "exec_server_remote"
    )]
    listen: Option<String>,

    #[command(flatten)]
    websocket_auth: WebsocketAuthArgs,

    /// Register this exec-server as a remote environment using the given base URL.
    #[arg(
        long = "remote",
        id = "exec_server_remote",
        value_name = "URL",
        requires = "environment_id",
        global = true
    )]
    pub(super) remote: Option<String>,

    /// Environment id to attach to when registering remotely.
    #[arg(long = "environment-id", value_name = "ID", global = true)]
    pub(super) environment_id: Option<String>,

    /// Human-readable environment name.
    #[arg(long = "name", value_name = "NAME", global = true)]
    pub(super) name: Option<String>,

    #[arg(
        help = "Use Agent Identity auth from CODEX_ACCESS_TOKEN for remote registration",
        long = "use-agent-identity-auth",
        requires = "exec_server_remote",
        global = true
    )]
    pub(super) use_agent_identity_auth: bool,

    /// Read one registration bearer from stdin.
    #[arg(
        long,
        requires = "exec_server_remote",
        conflicts_with = "use_agent_identity_auth",
        global = true
    )]
    registration_bearer_stdin: bool,

    /// Read rotating executor credentials from stdin; exit when the pipe closes.
    #[arg(
        long = "executor-credential-stdin",
        requires = "exec_server_remote",
        conflicts_with = "use_agent_identity_auth",
        global = true
    )]
    pub(super) executor_credential_stdin: bool,
    #[arg(
        help = "Read a ChatGPT access token from stdin for HTTPS registration",
        long,
        requires = "exec_server_remote",
        conflicts_with_all = ["use_agent_identity_auth", "executor_credential_stdin", "registration_bearer_stdin"]
    )]
    chatgpt_token_stdin: bool,

    #[arg(
        help = "ChatGPT account ID. Must match the token's account claim when present",
        long,
        value_name = "ID",
        requires = "chatgpt_token_stdin"
    )]
    chatgpt_account_id: Option<String>,

    /// Exit when the parent-owned standard-input pipe closes.
    #[arg(
        long = "exit-on-stdin-close",
        env = codex_exec_server::CODEX_EXEC_SERVER_EXIT_ON_STDIN_CLOSE_ENV_VAR,
        requires_if("true", "exec_server_remote"),
        global = true
    )]
    exit_on_stdin_close: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RemoteAuth {
    Default,
    AgentIdentity,
    ExecutorCredentialStdin,
    RegistrationBearerStdin,
    ChatgptStdin,
}

impl ExecServerCommand {
    /// Starts the executor with the given runtime paths and configuration overrides.
    pub(super) async fn run(
        mut self,
        arg0_paths: &Arg0DispatchPaths,
        root_config_overrides: &CliConfigOverrides,
    ) -> anyhow::Result<()> {
        let strict_config = self.strict_config;
        if self.registration_bearer_stdin && self.executor_credential_stdin {
            anyhow::bail!(
                "--registration-bearer-stdin cannot be used with --executor-credential-stdin"
            );
        }
        anyhow::ensure!(
            !self.chatgpt_token_stdin || self.command.is_none(),
            "--chatgpt-token-stdin cannot be used with forward"
        );
        let remote_auth = self.remote_auth();
        let enable_cloud_config = self.should_load_cloud_config();
        let websocket_auth = self.websocket_auth.try_into_settings()?;
        if websocket_auth.config.is_some() && (self.remote.is_some() || self.command.is_some()) {
            anyhow::bail!("WebSocket listener auth cannot be used with --remote or forward");
        }
        let codex_self_exe = arg0_paths
            .codex_self_exe
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Codex executable path is not configured"))?;
        let mut runtime_paths = ExecServerRuntimeOptions::new(
            codex_self_exe,
            arg0_paths.codex_linux_sandbox_exe.clone(),
        )?
        .with_linux_sandbox_pid_namespace(self.linux_sandbox_pid_namespace)
        .with_proxy_private_ips_via_upstream(self.proxy_private_ips_via_upstream);
        // Config loading below owns error handling (non-strict stdio can proceed
        // without config). Never retain other startup flags in the executor.
        if let Ok(overrides) = root_config_overrides.parse_overrides() {
            let cli_flags = codex_config::build_cli_overrides_layer(&overrides);
            runtime_paths.prefer_mxc = cli_flags
                .get("features")
                .and_then(|features| features.get("prefer_mxc"))
                .and_then(toml::Value::as_bool);
        }
        if let Some(base_url) = self.remote.take() {
            let environment_id = self.environment_id.take().ok_or_else(|| {
                anyhow::anyhow!("--environment-id is required when --remote is set")
            })?;
            let registration_auth = self
                .registration_bearer_stdin
                .then(codex_exec_server::registration_auth_from_stdin)
                .transpose()?;
            let stdin_auth = if self.chatgpt_token_stdin {
                codex_http_client::validate_https_endpoint_url(&base_url, "ChatGPT token registry")
                    .map_err(anyhow::Error::msg)?;
                Some(read_chatgpt_stdin_auth(self.chatgpt_account_id.take())?)
            } else {
                None
            };
            let config = Box::pin(load_exec_server_config(
                root_config_overrides,
                strict_config,
                enable_cloud_config,
                stdin_auth.as_ref(),
            ));
            let mut parent_lifetime = if self.exit_on_stdin_close && stdin_auth.is_some() {
                ParentLifetime::MonitoredStdin(exec_server_telemetry::monitor_stdin_close())
            } else if self.exit_on_stdin_close {
                ParentLifetime::StdinPipe
            } else {
                ParentLifetime::Independent
            };
            let config = match &mut parent_lifetime {
                ParentLifetime::MonitoredStdin(closed) => tokio::select! {
                    biased;
                    _ = closed => return Ok(()),
                    config = config => config?,
                },
                ParentLifetime::StdinPipe | ParentLifetime::Independent => config.await?,
            };
            // Configured exporters may contain account-wide credentials or client keys.
            // The scoped executor must not initialize those exporters.
            let telemetry_config =
                (remote_auth != RemoteAuth::ExecutorCredentialStdin).then_some(&config);
            let (_otel, telemetry) = exec_server_telemetry::init(telemetry_config);
            let mut credential_provider = None;
            let auth_provider: codex_api::SharedAuthProvider = match remote_auth {
                RemoteAuth::ExecutorCredentialStdin => {
                    let ExecutorCredentialPipe {
                        provider,
                        parent_closed,
                    } = ExecutorCredentialPipe::from_stdin(environment_id.trim().to_string());
                    provider.wait_for_initial().await?;
                    parent_lifetime = ParentLifetime::MonitoredStdin(parent_closed);
                    credential_provider = Some(Arc::clone(&provider));
                    provider
                }
                RemoteAuth::ChatgptStdin => {
                    let auth = stdin_auth
                        .as_ref()
                        .context("missing captured ChatGPT stdin authentication")?;
                    anyhow::ensure!(
                        config.auth_config().allows_auth(auth),
                        "ChatGPT authentication is not permitted for this account"
                    );
                    codex_model_provider::auth_provider_from_auth(auth)
                }
                RemoteAuth::RegistrationBearerStdin => {
                    registration_auth.context("missing captured registration bearer")?
                }
                RemoteAuth::AgentIdentity => {
                    load_exec_server_agent_identity_auth_provider(&config).await?
                }
                RemoteAuth::Default => {
                    load_exec_server_remote_auth_provider(&config, &base_url).await?
                }
            };
            let mut remote_config = codex_exec_server::RemoteEnvironmentConfig::new(
                base_url,
                environment_id,
                auth_provider,
                config.http_client_factory(),
            )?;
            if remote_auth == RemoteAuth::ExecutorCredentialStdin {
                remote_config = remote_config.with_executor_credential_auth()?;
            } else if self.chatgpt_token_stdin {
                remote_config = remote_config.with_registry_bearer_redaction();
            }
            if let Some(name) = self.name {
                remote_config.name = name;
            }
            remote_config.request_dispatch_mode = self.request_dispatch_mode;
            let remote_config = remote_config.with_telemetry(telemetry);
            let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel();
            #[cfg(target_os = "macos")]
            let runtime_paths = runtime_paths.with_allowed_symlinked_codex_home(
                codex_config::allowed_symlinked_codex_home(
                    &config.config_layer_stack,
                    &config.codex_home,
                ),
            );
            let result = Box::pin(exec_server_telemetry::run_until_shutdown(
                async move {
                    let shutdown = async move {
                        let _ = shutdown_receiver.await;
                    };
                    match self.command {
                        Some(ExecServerSubcommand::Forward { connect }) => {
                            codex_exec_server::run_remote_environment_forward_until_shutdown(
                                remote_config,
                                connect,
                                shutdown,
                            )
                            .await
                        }
                        None => {
                            codex_exec_server::run_remote_environment_until_shutdown(
                                remote_config,
                                runtime_paths,
                                shutdown,
                            )
                            .await
                        }
                    }
                    .map_err(anyhow::Error::new)
                },
                parent_lifetime,
                exec_server_telemetry::ShutdownBehavior::Graceful(shutdown_sender),
            ))
            .await;
            if let Some(provider) = credential_provider {
                provider.check_pipe_failure()?;
            }
            result
        } else {
            let config_result = Box::pin(load_exec_server_config(
                root_config_overrides,
                strict_config,
                enable_cloud_config,
                /*stdin_auth*/ None,
            ))
            .await;
            let config = if strict_config {
                Some(config_result?)
            } else {
                config_result.ok()
            };
            let (_otel, telemetry) = exec_server_telemetry::init(config.as_ref());
            #[cfg(target_os = "macos")]
            let runtime_paths = runtime_paths.with_allowed_symlinked_codex_home(
                config.as_ref().and_then(|config| {
                    codex_config::allowed_symlinked_codex_home(
                        &config.config_layer_stack,
                        &config.codex_home,
                    )
                }),
            );
            let http_client_factory = config.as_ref().map_or_else(
                || HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
                Config::http_client_factory,
            );
            let listen_url = self
                .listen
                .unwrap_or_else(|| codex_exec_server::DEFAULT_LISTEN_URL.to_string());
            let run = exec_server_telemetry::run_until_shutdown(
                codex_exec_server::run_main_with_telemetry(
                    &listen_url,
                    runtime_paths,
                    telemetry,
                    http_client_factory,
                    self.request_dispatch_mode,
                    websocket_auth,
                ),
                ParentLifetime::Independent,
                exec_server_telemetry::ShutdownBehavior::Immediate,
            );
            run.await.map_err(anyhow::Error::from_boxed)
        }
    }

    fn remote_auth(&self) -> RemoteAuth {
        match (
            self.use_agent_identity_auth,
            self.executor_credential_stdin,
            self.registration_bearer_stdin,
            self.chatgpt_token_stdin,
        ) {
            (true, false, false, false) => RemoteAuth::AgentIdentity,
            (false, true, false, false) => RemoteAuth::ExecutorCredentialStdin,
            (false, false, true, false) => RemoteAuth::RegistrationBearerStdin,
            (false, false, false, true) => RemoteAuth::ChatgptStdin,
            (false, false, false, false) => RemoteAuth::Default,
            _ => unreachable!("clap rejects conflicting exec-server authentication flags"),
        }
    }

    pub(super) fn should_load_cloud_config(&self) -> bool {
        self.remote.is_some()
            && self.command.is_none()
            && matches!(
                self.remote_auth(),
                RemoteAuth::Default | RemoteAuth::ChatgptStdin
            )
    }
}

#[derive(Debug, clap::Subcommand)]
pub(super) enum ExecServerSubcommand {
    /// Register an existing WebSocket exec-server as a remote environment.
    Forward {
        /// Destination exec-server WebSocket URL.
        #[arg(long, value_name = "URL", requires = "exec_server_remote")]
        connect: String,
    },
}

async fn load_exec_server_agent_identity_auth_provider(
    config: &Config,
) -> anyhow::Result<codex_api::SharedAuthProvider> {
    read_codex_access_token_from_env().ok_or_else(|| {
        anyhow::anyhow!("CODEX_ACCESS_TOKEN is required when --use-agent-identity-auth is set")
    })?;
    let auth = AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false)
        .await?
        .auth()
        .await
        .ok_or_else(|| anyhow::anyhow!("Agent Identity authentication is unavailable"))?;
    if !matches!(auth, CodexAuth::AgentIdentity(_)) {
        anyhow::bail!("CODEX_ACCESS_TOKEN did not provide permitted Agent Identity authentication");
    }
    Ok(codex_model_provider::auth_provider_from_auth(&auth))
}

async fn load_exec_server_remote_auth_provider(
    config: &Config,
    base_url: &str,
) -> anyhow::Result<codex_api::SharedAuthProvider> {
    let (auth_manager, auth) = load_exec_server_remote_auth(
        config,
        "remote exec-server registration requires ChatGPT authentication or API key authentication; run `codex login` or set CODEX_API_KEY",
    )
    .await?;

    if !is_supported_exec_server_remote_auth(&auth) {
        anyhow::bail!(
            "remote exec-server registration requires ChatGPT authentication or API key authentication; Agent Identity auth requires --use-agent-identity-auth"
        );
    }

    if auth.is_api_key_auth() {
        validate_api_key_remote_host(base_url)?;
    }

    if auth.uses_codex_backend() || auth_manager.is_workload_identity_selected() {
        Ok(registry_auth::reloading_auth(auth_manager, &auth))
    } else {
        Ok(codex_model_provider::auth_provider_from_auth(&auth))
    }
}

pub(super) fn is_supported_exec_server_remote_auth(auth: &CodexAuth) -> bool {
    auth.is_chatgpt_auth() || auth.is_api_key_auth()
}

pub(super) fn validate_api_key_remote_host(base_url: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(base_url)
        .map_err(|err| anyhow::anyhow!("invalid remote exec-server registration URL: {err}"))?;
    let host = url.host().ok_or_else(|| {
        anyhow::anyhow!("remote exec-server registration URL must include a host")
    })?;

    let is_loopback = match &host {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    };
    let is_openai_host = match &host {
        url::Host::Domain(host) => ["openai.com", "openai.org"].into_iter().any(|domain| {
            host.eq_ignore_ascii_case(domain)
                || host.to_ascii_lowercase().ends_with(&format!(".{domain}"))
        }),
        _ => false,
    };
    let is_allowed = match url.scheme() {
        "https" => is_loopback || is_openai_host,
        "http" => is_loopback,
        _ => false,
    };

    if !is_allowed {
        anyhow::bail!(
            "remote exec-server API-key authentication is restricted to HTTPS openai.com and openai.org hosts and subdomains or loopback hosts"
        );
    }

    Ok(())
}

async fn load_exec_server_config(
    root_config_overrides: &CliConfigOverrides,
    strict_config: bool,
    enable_cloud_config: bool,
    stdin_auth: Option<&CodexAuth>,
) -> anyhow::Result<Config> {
    let cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let bootstrap_cli_overrides = cli_kv_overrides.clone();
    let mut builder = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .strict_config(strict_config);
    if enable_cloud_config {
        let codex_home = find_codex_home()?;
        let loader_overrides = LoaderOverrides::default();
        let network_policy = EmbeddedNetworkPolicy::load(&loader_overrides).await;
        let bootstrap_cwd = AbsolutePathBuf::current_dir()?;
        let bootstrap_config = load_config_toml_with_layer_stack(
            &codex_home,
            Some(&bootstrap_cwd),
            bootstrap_cli_overrides,
            ConfigLoadOptions {
                loader_overrides,
                strict_config,
                cloud_config_bundle: CloudConfigBundleLoader::default(),
            },
        )
        .await?;
        let bootstrap_auth_config = bootstrap_auth_config(&codex_home, &bootstrap_config)?;
        let bootstrap_auth_config = network_policy.bind_bootstrap_auth(bootstrap_auth_config);
        let cloud_config_bundle = if let Some(auth) = stdin_auth {
            anyhow::ensure!(
                bootstrap_auth_config.allows_auth(auth),
                "ChatGPT authentication is not permitted for this account"
            );
            Box::pin(managed_config_bundle_loader_for_auth(
                bootstrap_auth_config,
                auth,
                bootstrap_config.config_layer_stack.requirements_toml(),
            ))
            .await?
        } else {
            let auth_manager = AuthManager::shared_from_auth_config_with_auth_modes(
                bootstrap_auth_config.clone(),
                /*enable_codex_api_key_env*/ true,
                vec![
                    codex_protocol::auth::AuthMode::ApiKey,
                    codex_protocol::auth::AuthMode::Chatgpt,
                    codex_protocol::auth::AuthMode::ChatgptAuthTokens,
                    codex_protocol::auth::AuthMode::PersonalAccessToken,
                ],
            )
            .await?;
            anyhow::ensure!(
                auth_manager.auth_cached().is_some(),
                "remote exec-server registration requires ChatGPT authentication or API key authentication; Agent Identity auth requires --use-agent-identity-auth"
            );
            chatgpt_config_bundle_loader(
                auth_manager,
                bootstrap_auth_config
                    .chatgpt_base_url
                    .clone()
                    .unwrap_or_else(|| "https://chatgpt.com/backend-api/".into()),
                bootstrap_auth_config.codex_home,
                bootstrap_auth_config
                    .auth_route_config
                    .http_client_factory()
                    .clone(),
            )
        };
        builder = builder.cloud_config_bundle(cloud_config_bundle);
    }
    let mut config = builder.build().await?;
    if stdin_auth.is_some() {
        config.config_layer_stack.ensure_config_server_applied()?;
    }
    EmbeddedNetworkPolicy::default().activate(&mut config);
    Ok(config)
}

async fn load_exec_server_remote_auth(
    config: &Config,
    missing_auth_error: &'static str,
) -> anyhow::Result<(Arc<AuthManager>, CodexAuth)> {
    let auth_manager =
        AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ true).await?;

    let auth = if let Some(auth) = auth_manager.auth().await {
        auth
    } else {
        auth_manager.reload().await;
        auth_manager
            .auth()
            .await
            .ok_or_else(|| anyhow::anyhow!(missing_auth_error))?
    };

    Ok((auth_manager, auth))
}

fn read_chatgpt_stdin_auth(account_id: Option<String>) -> anyhow::Result<CodexAuth> {
    const MAX_FRAME_BYTES: u64 = 64 * 1024;
    let stdin = codex_exec_server::credential_stdin()?;
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        use std::os::unix::fs::FileTypeExt;
        let kind = std::fs::File::from(stdin.as_fd().try_clone_to_owned()?)
            .metadata()?
            .file_type();
        anyhow::ensure!(
            kind.is_fifo() || kind.is_socket(),
            "ChatGPT token stdin must be a pipe or socket"
        );
    }
    #[cfg(not(unix))]
    anyhow::ensure!(
        !std::io::IsTerminal::is_terminal(&stdin),
        "ChatGPT token stdin must be piped"
    );

    // The launcher keeps the pipe open for --exit-on-stdin-close after this line.
    let mut line = String::new();
    stdin.lock().take(MAX_FRAME_BYTES).read_line(&mut line)?;
    chatgpt_auth_from_line(&line, account_id)
}

fn chatgpt_auth_from_line(line: &str, account_id: Option<String>) -> anyhow::Result<CodexAuth> {
    let token = line
        .strip_suffix('\n')
        .context("expected a bounded, newline-terminated ChatGPT access token")?;
    let token = token.strip_suffix('\r').unwrap_or(token);
    anyhow::ensure!(
        !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic()),
        "invalid ChatGPT access token"
    );
    let claimed_id = codex_login::token_data::parse_chatgpt_jwt_claims(token)
        .map_err(|_| anyhow::anyhow!("invalid ChatGPT access token"))?
        .chatgpt_account_id;
    if let (Some(id), Some(claim)) = (&account_id, &claimed_id) {
        anyhow::ensure!(id == claim, "--chatgpt-account-id does not match the token");
    }
    // A token without an account claim cannot establish the supplied account's plan.
    let plan_override = claimed_id.is_none().then_some("unknown");
    let account_id = account_id
        .or(claimed_id)
        .context("token has no ChatGPT account ID; supply --chatgpt-account-id")?;
    anyhow::ensure!(
        !account_id.is_empty() && account_id.bytes().all(|b| b.is_ascii_graphic()),
        "invalid ChatGPT account ID"
    );
    CodexAuth::from_external_chatgpt_tokens(token, &account_id, plan_override)
        .map_err(|_| anyhow::anyhow!("invalid ChatGPT access token"))
}
