use std::collections::HashMap;
#[cfg(windows)]
use std::path::Path;
use std::sync::Arc;

use crate::process_telemetry::ProcessTelemetry;
use codex_exec_server_protocol::JSONRPCErrorError;
use codex_exec_server_protocol::ProcessStartErrorData;
use codex_file_system::WindowsSandboxSelection;
use codex_network_proxy::CUSTOM_CA_ENV_KEYS;
use codex_network_proxy::NetworkPolicyAuditObserver;
use codex_network_proxy::NetworkPolicyDecider;
use codex_network_proxy::NetworkProxyHandle;
use codex_network_proxy::RemoteCredentialBrokerCache;
use codex_network_proxy::is_managed_mitm_ca_trust_bundle_path;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxNetwork;
use codex_sandboxing::SandboxTransformRequest;
use codex_sandboxing::WrappedSandboxCommand;
#[cfg(windows)]
use codex_shell_command::shell_detect::fallback_powershell_shell_for_windows_sandbox;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;

#[cfg(unix)]
use crate::CODEX_ARG0_EXEC_HELPER_ARG1;
use crate::ExecServerRuntimeOptions;
use crate::process_telemetry::trace_process_id;
use crate::protocol::ExecParams;
use crate::rpc::invalid_params;
use crate::sandbox_selection::sandbox_selection;

#[tracing::instrument(
    name = "codex.exec_server.process_prepare_sandbox",
    skip_all,
    fields(process.id = trace_process_id(params.process_id.as_str())),
)]
pub(crate) async fn prepare_exec_request_with_telemetry(
    params: &ExecParams,
    env: HashMap<String, String>,
    runtime_paths: Option<&ExecServerRuntimeOptions>,
    _network_policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
    _network_policy_audit_observer: Option<NetworkPolicyAuditObserver>,
    _credential_brokers: &RemoteCredentialBrokerCache,
    _telemetry: &ProcessTelemetry,
) -> Result<(WrappedSandboxCommand, Option<NetworkProxyHandle>), JSONRPCErrorError> {
    if let Some(sandbox) = params.sandbox.as_ref()
        && sandbox.windows_sandbox_selection == WindowsSandboxSelection::Mxc
    {
        if params.arg0.is_some() {
            return Err(invalid_params(
                "MXC custom argv0 is not supported".to_owned(),
            ));
        }
        if !codex_sandboxing::windows_mxc_available() {
            return Err(
                ProcessStartErrorData::SandboxUnavailable.attach_to(invalid_params(
                    "native MXC is unavailable on this executor".to_owned(),
                )),
            );
        }
    }
    // TELOS(removed-stacks): never downgrade an executor-local proxy request.
    if params.network_proxy.is_some() {
        return Err(invalid_params("network proxy is not supported".to_string()));
    }
    let managed_network = params.managed_network.clone();
    let Some(sandbox_context) = params.sandbox.as_ref() else {
        if params.enforce_managed_network {
            return Err(
                ProcessStartErrorData::SandboxUnavailable.attach_to(invalid_params(
                    "managed network enforcement requires a configured sandbox".to_string(),
                )),
            );
        }
        validate_native_cwd(&params.cwd)?;
        let prepared = WrappedSandboxCommand::native(
            params.argv.clone(),
            params.cwd.clone(),
            env,
            params.arg0.clone(),
        )
        .map_err(|err| invalid_params(err.to_string()))?;
        return Ok((prepared, None));
    };
    let runtime_paths = runtime_paths.ok_or_else(|| {
        ProcessStartErrorData::SandboxUnavailable.attach_to(invalid_params(
            "sandbox runtime paths are not configured".to_string(),
        ))
    })?;
    let mut runtime_readable_paths = Vec::new();
    if cfg!(target_os = "linux") || (cfg!(unix) && params.arg0.is_some()) {
        runtime_readable_paths.push(runtime_paths.codex_self_exe.clone());
    }
    if cfg!(target_os = "linux") {
        runtime_readable_paths.extend(runtime_paths.codex_linux_sandbox_exe.iter().cloned());
    }
    let managed_mitm_ca_trust_bundle_path = if managed_network.is_some() {
        CUSTOM_CA_ENV_KEYS.into_iter().find_map(|key| {
            let path = env.get(key)?;
            if !is_managed_mitm_ca_trust_bundle_path(path) {
                return None;
            }
            AbsolutePathBuf::from_absolute_path(path).ok()
        })
    } else {
        None
    };
    runtime_readable_paths.extend(managed_mitm_ca_trust_bundle_path);
    let sandbox_manager = SandboxManager::new()
        .with_linux_sandbox_pid_namespace(runtime_paths.linux_sandbox_pid_namespace);
    #[cfg(windows)]
    let sandbox_manager = sandbox_manager.with_windows_token_session(
        sandbox_context
            .windows_sandbox_proxy_settings_mode
            .unwrap_or_default(),
    );
    #[cfg(target_os = "macos")]
    let sandbox_manager = sandbox_manager
        .with_allowed_symlinked_codex_home(runtime_paths.allowed_symlinked_codex_home.clone());
    let (selection, windows_sandbox_level) = sandbox_selection(sandbox_context);
    let (program, args) = params
        .argv
        .split_first()
        .ok_or_else(|| invalid_params("argv must not be empty".to_string()))?;
    #[cfg(unix)]
    let (program, args) = params.arg0.as_ref().map_or_else(
        || (program.into(), args.to_vec()),
        |arg0| {
            let mut helper_args = Vec::with_capacity(params.argv.len() + 2);
            helper_args.push(CODEX_ARG0_EXEC_HELPER_ARG1.to_string());
            helper_args.push(arg0.clone());
            helper_args.extend(params.argv.iter().cloned());
            (
                runtime_paths
                    .codex_self_exe
                    .as_path()
                    .as_os_str()
                    .to_owned(),
                helper_args,
            )
        },
    );
    #[cfg(not(unix))]
    let (program, args) = (program.into(), args.to_vec());
    #[cfg(windows)]
    let program = if matches!(
        sandbox_context.windows_sandbox_selection,
        WindowsSandboxSelection::Elevated | WindowsSandboxSelection::Mxc
    ) && Path::new(&program).file_stem().is_some_and(|name| {
        name.eq_ignore_ascii_case("pwsh") || name.eq_ignore_ascii_case("powershell")
    }) && let Some(fallback) =
        fallback_powershell_shell_for_windows_sandbox(Path::new(&program))
    {
        // Remote controllers cannot resolve a sandbox-compatible shell on this host.
        fallback.shell_path.into_os_string()
    } else {
        program
    };
    let prepared = sandbox_manager
        .transform(SandboxTransformRequest {
            command: SandboxCommand {
                program,
                args,
                cwd: params.cwd.clone(),
                env,
                managed_network,
                additional_permissions: None,
            },
            permissions: &sandbox_context.permissions,
            workspace_roots: Some(&sandbox_context.workspace_roots),
            runtime_readable_paths: Some(&runtime_readable_paths),
            selection,
            network: SandboxNetwork::new(
                params.enforce_managed_network,
                /*proxy*/ None,
                /*environment_id*/ None,
            ),
            sandbox_policy_cwd: &sandbox_context.cwd,
            sandbox_exe: if cfg!(windows) {
                Some(runtime_paths.codex_self_exe.as_path())
            } else {
                runtime_paths.codex_linux_sandbox_exe.as_deref()
            },
            use_legacy_landlock: sandbox_context.use_legacy_landlock,
            windows_sandbox_level,
        })
        .map_err(|err| match err {
            codex_sandboxing::SandboxTransformError::SandboxUnavailable => {
                ProcessStartErrorData::SandboxUnavailable.attach_to(invalid_params(err.to_string()))
            }
            err => sandbox_preparation_error(err),
        })?;
    crate::run_integrity_checks(prepared.integrity_request(sandbox_context.sandbox_override)).await;
    Ok((prepared, None))
}

fn sandbox_preparation_error(err: impl std::fmt::Display) -> JSONRPCErrorError {
    ProcessStartErrorData::SandboxPreparation.attach_to(invalid_params(format!(
        "failed to prepare process sandbox: {err}"
    )))
}

pub(crate) fn validate_native_cwd(cwd: &PathUri) -> Result<(), JSONRPCErrorError> {
    cwd.to_abs_path().map_err(|err| {
        invalid_params(format!(
            "cwd URI `{cwd}` is not valid on this exec-server host: {err}"
        ))
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "process_sandbox_tests.rs"]
mod tests;
