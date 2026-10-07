//! Serves the in-process app-server behind the embedding contract.

use std::io::Result as IoResult;
use std::sync::Arc;

use codex_app_server_embedding::EmbeddedConnection;
use codex_app_server_embedding::EmbeddedServer;
use codex_app_server_embedding::InProcessClientStartArgs;
use codex_app_server_embedding::InProcessServerEvent;
use codex_app_server_embedding::RequestResult;
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
        let initialize = args.initialize_params();
        let handle = crate::in_process::start(InProcessStartArgs {
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
        })
        .await?;
        Ok(Box::new(handle) as Box<dyn EmbeddedServer>)
    })
}

impl EmbeddedServer for InProcessClientHandle {
    fn connection(&self) -> Arc<dyn EmbeddedConnection> {
        Arc::new(self.sender())
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
