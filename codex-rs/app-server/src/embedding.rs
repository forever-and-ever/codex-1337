//! Serves the in-process app-server behind the embedding contract.

use std::io::Result as IoResult;
use std::sync::Arc;

use codex_app_server_embedding::EmbeddedConnection;
use codex_app_server_embedding::EmbeddedServer;
use codex_app_server_embedding::InProcessClientStartArgs;
use codex_app_server_embedding::InProcessServerEvent;
use codex_app_server_embedding::RequestResult;
use codex_app_server_embedding::ThreadPermissionsReader;
use codex_app_server_protocol::ClientNotification;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::Result as JsonRpcResult;
use codex_config::NoopThreadConfigLoader;
use futures::future::BoxFuture;

use crate::in_process::InProcessClientHandle;
use crate::in_process::InProcessClientSender;
use crate::in_process::InProcessStartArgs;

/// Lets clients in this process start an embedded app-server.
pub fn install_embedded_runtime() {
    codex_app_server_embedding::install(start);
}

fn start(args: InProcessClientStartArgs) -> BoxFuture<'static, IoResult<Box<dyn EmbeddedServer>>> {
    Box::pin(async move {
        let handle = crate::in_process::start(runtime_start_args(args)).await?;
        Ok(Box::new(handle) as Box<dyn EmbeddedServer>)
    })
}

fn runtime_start_args(args: InProcessClientStartArgs) -> InProcessStartArgs {
    let initialize = args.initialize_params();
    InProcessStartArgs {
        arg0_paths: args.arg0_paths,
        config: args.config,
        cli_overrides: args.cli_overrides,
        loader_overrides: args.loader_overrides,
        strict_config: args.strict_config,
        cloud_config_bundle: args.cloud_config_bundle,
        embedded_network_policy: args.embedded_network_policy,
        thread_config_loader: Arc::new(NoopThreadConfigLoader),
        feedback: args.feedback,
        log_db: args.log_db,
        state_db: args.state_db,
        environment_manager: args.environment_manager,
        config_warnings: args.config_warnings,
        session_source: args.session_source,
        enable_codex_api_key_env: args.enable_codex_api_key_env,
        initialize,
        channel_capacity: args.channel_capacity,
    }
}

impl EmbeddedServer for InProcessClientHandle {
    fn connection(&self) -> Arc<dyn EmbeddedConnection> {
        Arc::new(self.sender())
    }

    fn thread_permissions_reader(&self) -> ThreadPermissionsReader {
        let reader = InProcessClientHandle::thread_permissions_reader(self);
        Arc::new(move |thread_id| {
            let reader = reader.clone();
            Box::pin(async move { reader.read(thread_id).await })
        })
    }

    fn next_event(&mut self) -> BoxFuture<'_, Option<InProcessServerEvent>> {
        Box::pin(InProcessClientHandle::next_event(self))
    }

    fn shutdown(self: Box<Self>) -> BoxFuture<'static, IoResult<()>> {
        Box::pin(InProcessClientHandle::shutdown(*self))
    }
}

impl EmbeddedConnection for InProcessClientSender {
    fn request(&self, request: ClientRequest) -> BoxFuture<'static, IoResult<RequestResult>> {
        let sender = self.clone();
        Box::pin(async move { sender.request(request).await })
    }

    fn notify(&self, notification: ClientNotification) -> IoResult<()> {
        InProcessClientSender::notify(self, notification)
    }

    fn respond_to_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> IoResult<()> {
        InProcessClientSender::respond_to_server_request(self, request_id, result)
    }

    fn fail_server_request(&self, request_id: RequestId, error: JSONRPCErrorError) -> IoResult<()> {
        InProcessClientSender::fail_server_request(self, request_id, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_embedding::EmbeddedNetworkPolicy;
    use codex_arg0::Arg0DispatchPaths;
    use codex_config::CloudConfigBundleLoader;
    use codex_config::LoaderOverrides;
    use codex_core::config::ConfigBuilder;
    use codex_exec_server::EnvironmentManager;
    use codex_exec_server::ExecServerRuntimeOptions;
    use codex_feedback::CodexFeedback;
    use codex_protocol::protocol::SessionSource;

    #[tokio::test]
    async fn runtime_start_args_forward_environment_manager_and_capabilities() {
        let home = tempfile::tempdir().expect("temporary Codex home");
        let config = Arc::new(
            ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .fallback_cwd(Some(home.path().to_path_buf()))
                .build()
                .await
                .expect("test config"),
        );
        let environment_manager = Arc::new(
            EnvironmentManager::create_for_tests(
                Some("ws://127.0.0.1:8765".to_string()),
                Some(
                    ExecServerRuntimeOptions::new(
                        std::env::current_exe().expect("current exe"),
                        /*codex_linux_sandbox_exe*/ None,
                    )
                    .expect("runtime paths"),
                ),
            )
            .await,
        );
        let runtime_args = runtime_start_args(InProcessClientStartArgs {
            required_human_approval: true,
            arg0_paths: Arg0DispatchPaths::default(),
            config: Arc::clone(&config),
            cli_overrides: Vec::new(),
            loader_overrides: LoaderOverrides::default(),
            strict_config: false,
            cloud_config_bundle: CloudConfigBundleLoader::default(),
            embedded_network_policy: EmbeddedNetworkPolicy::default(),
            feedback: CodexFeedback::new(),
            log_db: None,
            state_db: None,
            environment_manager: Arc::clone(&environment_manager),
            config_warnings: Vec::new(),
            session_source: SessionSource::Exec,
            enable_codex_api_key_env: false,
            client_name: "codex-app-server-embedding-test".to_string(),
            client_version: "0.0.0-test".to_string(),
            experimental_api: true,
            mcp_server_openai_form_elicitation: true,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: crate::in_process::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
        });
        assert_eq!(runtime_args.config, config);
        let capabilities = runtime_args
            .initialize
            .capabilities
            .expect("initialize capabilities");
        assert!(capabilities.required_human_approval);
        assert!(capabilities.mcp_server_openai_form_elicitation);
        assert!(Arc::ptr_eq(
            &runtime_args.environment_manager,
            &environment_manager
        ));
        assert!(
            runtime_args
                .environment_manager
                .default_environment()
                .expect("default environment")
                .is_remote()
        );
    }
}
