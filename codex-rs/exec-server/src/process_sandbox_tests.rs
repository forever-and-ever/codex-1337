use std::collections::HashMap;
use std::sync::Arc;

use codex_exec_server_protocol::JSONRPCErrorError;
#[cfg(unix)]
use codex_file_system::WindowsSandboxSelection;
#[cfg(target_os = "macos")]
use codex_network_proxy::ManagedNetworkSandboxContext;
use codex_network_proxy::NetworkPolicyAuditObserver;
use codex_network_proxy::NetworkPolicyDecider;
use codex_network_proxy::NetworkProxyConfig;
use codex_network_proxy::NetworkProxyHandle;
use codex_network_proxy::RemoteNetworkProxyConfig;
use codex_network_proxy::RemoteNetworkProxyLaunchConfig;
#[cfg(any(unix, windows))]
use codex_protocol::models::PermissionProfile;
use codex_sandboxing::WrappedSandboxCommand;
#[cfg(target_os = "linux")]
use codex_sandboxing::landlock::CODEX_LINUX_SANDBOX_ARG0;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
#[cfg(unix)]
use tempfile::tempdir;

use super::prepare_exec_request_with_telemetry;
#[cfg(unix)]
use crate::CODEX_ARG0_EXEC_HELPER_ARG1;
use crate::ExecParams;
use crate::ExecServerRuntimeOptions;
#[cfg(any(unix, windows))]
use crate::FileSystemSandboxContext;
use crate::ProcessId;
use crate::process_telemetry::ProcessTelemetry;

async fn prepare_exec_request(
    params: &ExecParams,
    env: HashMap<String, String>,
    runtime_paths: Option<&ExecServerRuntimeOptions>,
    network_policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
    network_policy_audit_observer: Option<NetworkPolicyAuditObserver>,
) -> Result<(WrappedSandboxCommand, Option<NetworkProxyHandle>), JSONRPCErrorError> {
    prepare_exec_request_with_telemetry(
        params,
        env,
        runtime_paths,
        network_policy_decider,
        network_policy_audit_observer,
        &codex_network_proxy::RemoteCredentialBrokerCache::default(),
        &ProcessTelemetry::default(),
    )
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn sandbox_request_wraps_native_argv_on_executor() {
    let command_directory = tempdir().expect("command directory");
    let cwd = AbsolutePathBuf::from_absolute_path(command_directory.path()).expect("absolute cwd");
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let self_exe = std::env::current_exe().expect("current executable");
    let runtime_paths =
        ExecServerRuntimeOptions::new(self_exe.clone(), Some(self_exe)).expect("runtime paths");
    let sandbox = FileSystemSandboxContext::from_permission_profile(
        PermissionProfile::workspace_write(),
        cwd_uri.clone(),
    );
    let params = ExecParams {
        metadata: Default::default(),
        process_id: ProcessId::from("process-1"),
        argv: vec![
            "/bin/bash".to_string(),
            "-lc".to_string(),
            "pwd".to_string(),
        ],
        cwd: cwd_uri,
        shell_snapshot: None,
        env_policy: None,
        env: HashMap::new(),
        tty: false,
        pipe_stdin: false,
        arg0: None,
        sandbox: Some(sandbox),
        enforce_managed_network: false,
        managed_network: None,
        network_proxy: None,
    };

    let (prepared, _) = prepare_exec_request(
        &params,
        HashMap::new(),
        Some(&runtime_paths),
        /*network_policy_decider*/ None,
        /*network_policy_audit_observer*/ None,
    )
    .await
    .expect("prepare sandboxed request");
    let prepared = prepared.into_command_request().expect("command request");

    assert_ne!(prepared.command, params.argv);
    assert_eq!(prepared.cwd, params.cwd);
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            prepared.command.first(),
            Some(&runtime_paths.codex_self_exe.to_string_lossy().into_owned())
        );
        let permission_profile_json = prepared
            .command
            .iter()
            .position(|arg| arg == "--permission-profile")
            .and_then(|index| prepared.command.get(index + 1))
            .expect("sandbox wrapper permission profile");
        let permission_profile: PermissionProfile =
            serde_json::from_str(permission_profile_json).expect("permission profile JSON");
        assert_eq!(
            permission_profile,
            PermissionProfile::workspace_write()
                .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd))
        );

        let policy_directory = tempdir().expect("policy directory");
        let explicit_cwd =
            AbsolutePathBuf::from_absolute_path(policy_directory.path()).expect("policy cwd");
        let mut params = params.clone();
        for policy_cwd in [&cwd, &explicit_cwd] {
            params.sandbox.as_mut().expect("sandbox").cwd = PathUri::from_abs_path(policy_cwd);
            let (prepared, _) = prepare_exec_request(
                &params,
                HashMap::new(),
                Some(&runtime_paths),
                /*network_policy_decider*/ None,
                /*network_policy_audit_observer*/ None,
            )
            .await
            .expect("prepare sandboxed request");
            let prepared = prepared.into_command_request().expect("command request");
            let actual_cwds = prepared
                .command
                .windows(4)
                .find(|args| args[0] == "--sandbox-policy-cwd")
                .expect("sandbox wrapper cwd arguments");
            assert_eq!(
                actual_cwds,
                [
                    "--sandbox-policy-cwd",
                    policy_cwd.to_string_lossy().as_ref(),
                    "--command-cwd",
                    cwd.to_string_lossy().as_ref(),
                ]
            );
        }
    }
    #[cfg(target_os = "macos")]
    assert_eq!(
        prepared.command.first().map(String::as_str),
        Some("/usr/bin/sandbox-exec")
    );

    let mut params = params;
    params.sandbox.as_mut().unwrap().windows_sandbox_selection = WindowsSandboxSelection::Mxc;
    let error = prepare_exec_request(
        &params,
        HashMap::new(),
        Some(&runtime_paths),
        /*network_policy_decider*/ None,
        /*network_policy_audit_observer*/ None,
    )
    .await
    .err()
    .expect("unsupported MXC must fail closed");
    assert_eq!(error.message, "native MXC is unavailable on this executor");
}

#[cfg(unix)]
#[tokio::test]
async fn sandbox_request_routes_custom_arg0_to_inner_helper() {
    let cwd: AbsolutePathBuf = std::env::current_dir()
        .expect("current directory")
        .try_into()
        .expect("absolute cwd");
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let self_exe = std::env::current_exe().expect("current executable");
    let runtime_paths =
        ExecServerRuntimeOptions::new(self_exe.clone(), Some(self_exe)).expect("runtime paths");
    let sandbox = FileSystemSandboxContext::from_permission_profile(
        PermissionProfile::workspace_write(),
        cwd_uri.clone(),
    );
    let params = ExecParams {
        metadata: Default::default(),
        process_id: ProcessId::from("process-custom-arg0"),
        argv: vec!["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
        cwd: cwd_uri,
        shell_snapshot: None,
        env_policy: None,
        env: HashMap::new(),
        tty: false,
        pipe_stdin: false,
        arg0: Some("custom-arg0".to_string()),
        sandbox: Some(sandbox),
        enforce_managed_network: false,
        managed_network: None,
        network_proxy: None,
    };

    let (prepared, _) = prepare_exec_request(
        &params,
        HashMap::new(),
        Some(&runtime_paths),
        /*network_policy_decider*/ None,
        /*network_policy_audit_observer*/ None,
    )
    .await
    .expect("prepare sandboxed request");
    let prepared = prepared.into_command_request().expect("command request");
    let helper_mode = prepared
        .command
        .iter()
        .position(|arg| arg == CODEX_ARG0_EXEC_HELPER_ARG1)
        .expect("sandboxed command should invoke arg0 helper");

    assert_eq!(
        prepared.command[helper_mode..],
        [
            CODEX_ARG0_EXEC_HELPER_ARG1,
            "custom-arg0",
            "/bin/sh",
            "-c",
            "true",
        ]
    );
    #[cfg(target_os = "linux")]
    assert_eq!(prepared.arg0, Some(CODEX_LINUX_SANDBOX_ARG0.to_string()));
    #[cfg(target_os = "macos")]
    assert_eq!(prepared.arg0, None);
}

#[cfg(target_os = "macos")]
fn managed_network_sandbox_request() -> (ExecParams, ExecServerRuntimeOptions) {
    let cwd: AbsolutePathBuf = std::env::current_dir()
        .expect("current directory")
        .try_into()
        .expect("absolute cwd");
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let self_exe = std::env::current_exe().expect("current executable");
    let runtime_paths =
        ExecServerRuntimeOptions::new(self_exe.clone(), Some(self_exe)).expect("runtime paths");
    let sandbox = FileSystemSandboxContext::from_permission_profile(
        PermissionProfile::workspace_write(),
        cwd_uri.clone(),
    );
    let params = ExecParams {
        metadata: Default::default(),
        process_id: ProcessId::from("process-managed-network"),
        argv: vec!["/usr/bin/true".to_string()],
        cwd: cwd_uri,
        shell_snapshot: None,
        env_policy: None,
        env: HashMap::new(),
        tty: false,
        pipe_stdin: false,
        arg0: None,
        sandbox: Some(sandbox),
        enforce_managed_network: true,
        managed_network: None,
        network_proxy: None,
    };
    (params, runtime_paths)
}

#[cfg(target_os = "macos")]
fn seatbelt_policy_arg(command: &[String]) -> &str {
    command
        .windows(2)
        .find_map(|args| (args[0] == "-p").then_some(args[1].as_str()))
        .expect("Seatbelt policy argument")
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_request_preserves_prepared_managed_network_policy() {
    let (mut params, runtime_paths) = managed_network_sandbox_request();
    let socket_dir = tempfile::tempdir().expect("temporary socket directory");
    let allowed_socket = socket_dir
        .path()
        .canonicalize()
        .expect("canonical socket directory")
        .join("allowed.sock")
        .to_string_lossy()
        .into_owned();
    params.managed_network = Some(ManagedNetworkSandboxContext {
        loopback_ports: vec![43123],
        allow_local_binding: false,
        allow_unix_sockets: vec![allowed_socket.clone()],
        dangerously_allow_all_unix_sockets: false,
    });

    let (prepared, _) = prepare_exec_request(
        &params,
        HashMap::new(),
        Some(&runtime_paths),
        /*network_policy_decider*/ None,
        /*network_policy_audit_observer*/ None,
    )
    .await
    .expect("prepare managed-network sandbox request");
    let prepared = prepared.into_command_request().expect("command request");
    let policy = seatbelt_policy_arg(&prepared.command);
    let unix_socket_definitions = prepared
        .command
        .iter()
        .filter(|arg| arg.starts_with("-DUNIX_SOCKET_PATH_"))
        .cloned()
        .collect::<Vec<_>>();

    assert!(policy.contains("(allow network-outbound (remote ip \"localhost:43123\"))"));
    assert!(policy.contains("(allow system-socket (socket-domain AF_UNIX))"));
    assert!(policy.contains(
        "(allow network-outbound (remote unix-socket (subpath (param \"UNIX_SOCKET_PATH_0\"))))"
    ));
    assert_eq!(
        unix_socket_definitions,
        vec![format!("-DUNIX_SOCKET_PATH_0={allowed_socket}")]
    );
    assert!(!policy.contains("(allow network-outbound (remote unix-socket))"));
    assert!(!policy.contains("(allow network-outbound)\n"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_request_only_allows_all_unix_sockets_when_configured() {
    let (mut params, runtime_paths) = managed_network_sandbox_request();
    params.managed_network = Some(ManagedNetworkSandboxContext {
        loopback_ports: vec![43123],
        ..ManagedNetworkSandboxContext::default()
    });

    for allow_all in [false, true] {
        params
            .managed_network
            .as_mut()
            .expect("managed network context")
            .dangerously_allow_all_unix_sockets = allow_all;
        let (prepared, _) = prepare_exec_request(
            &params,
            HashMap::new(),
            Some(&runtime_paths),
            /*network_policy_decider*/ None,
            /*network_policy_audit_observer*/ None,
        )
        .await
        .expect("prepare managed-network sandbox request");
        let prepared = prepared.into_command_request().expect("command request");
        let policy = seatbelt_policy_arg(&prepared.command);

        assert_eq!(
            (
                policy.contains("(allow system-socket (socket-domain AF_UNIX))"),
                policy.contains("(allow network-bind (local unix-socket))"),
                policy.contains("(allow network-outbound (remote unix-socket))"),
            ),
            (allow_all, allow_all, allow_all)
        );
        assert!(!policy.contains("(allow network-outbound)\n"));
        assert!(
            !prepared
                .command
                .iter()
                .any(|arg| arg.starts_with("-DUNIX_SOCKET_PATH_"))
        );
    }
}

#[tokio::test]
async fn native_request_preserves_native_launch_fields() {
    let cwd: AbsolutePathBuf = std::env::current_dir()
        .expect("current directory")
        .try_into()
        .expect("absolute cwd");
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let env = HashMap::from([("TEST_ENV".to_string(), "value".to_string())]);
    let params = ExecParams {
        metadata: Default::default(),
        process_id: ProcessId::from("process-1"),
        argv: vec!["echo".to_string(), "hello".to_string()],
        cwd: cwd_uri,
        shell_snapshot: None,
        env_policy: None,
        env: HashMap::new(),
        tty: false,
        pipe_stdin: false,
        arg0: Some("custom-arg0".to_string()),
        sandbox: None,
        enforce_managed_network: false,
        managed_network: None,
        network_proxy: None,
    };

    let (prepared, _) = prepare_exec_request(
        &params,
        env.clone(),
        /*runtime_paths*/ None,
        /*network_policy_decider*/ None,
        /*network_policy_audit_observer*/ None,
    )
    .await
    .expect("prepare native request");
    let prepared = prepared.into_command_request().expect("command request");

    assert_eq!(prepared.command, params.argv);
    assert_eq!(prepared.cwd, params.cwd);
    assert_eq!(prepared.env, env);
    assert_eq!(prepared.arg0, params.arg0);
}

// Client capability negotiation cannot authorize a proxy through raw process/start.
#[tokio::test]
async fn retired_network_requests_cannot_downgrade_to_a_native_launch() {
    let mut params: ExecParams = serde_json::from_value(serde_json::json!({
        "processId": "removed-proxy",
        "argv": ["/bin/false"],
        "cwd": PathUri::from_host_native_path(std::env::current_dir().unwrap()).unwrap(),
        "env": {},
        "tty": false
    }))
    .unwrap();

    for transparent in [false, true] {
        let mut request = serde_json::to_value(&params).unwrap();
        request["managedNetwork"] = serde_json::json!({"transparentTcp": transparent});
        assert_eq!(
            serde_json::from_value::<ExecParams>(request).is_err(),
            transparent
        );
    }

    let proxy = RemoteNetworkProxyConfig::from_effective_config(&NetworkProxyConfig::default())
        .expect("inactive proxy wire configuration");
    params.network_proxy = Some(RemoteNetworkProxyLaunchConfig::new(proxy));
    let error = prepare_exec_request(&params, HashMap::new(), None, None, None)
        .await
        .err()
        .expect("executor-local proxy launch must be rejected");
    assert!(error.message.contains("network proxy is not supported"));

    params.network_proxy = None;
    params.enforce_managed_network = true;
    let error = prepare_exec_request(&params, HashMap::new(), None, None, None)
        .await
        .err()
        .expect("native launch must not erase managed network enforcement");
    assert!(
        error
            .message
            .contains("managed network enforcement requires")
    );
}
