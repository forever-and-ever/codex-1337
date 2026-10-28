//! Optional catalog discovery uses the public RPC and never prevents ordinary bootstrap.

use super::*;
use crate::legacy_core::config::ConfigBuilder;
use codex_app_server_protocol::JSONRPCMessage;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;
use codex_protocol::openai_models::ReasoningEffort;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn collaboration_catalog_is_optional_and_refetched_on_bootstrap() -> Result<()> {
    for initial_reply in [
        Some(json!({"error": {"code": -32601, "message": "Method not found"}})),
        Some(json!({"error": {"code": -32600, "message": "Experimental API unavailable"}})),
        Some(json!({"error": {"code": -32603, "message": "Discovery failed"}})),
        Some(json!({"result": {"data": []}})),
        Some(json!({"result": {"data": "invalid"}})),
        None,
    ] {
        let codex_home = tempfile::tempdir()?;
        let mut config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await?;
        config.model = Some("task-model".into());
        let mut expected_account_params = json!({
            "threadId": null,
            "newThread": {
                "modelProvider": null,
                "cwd": null,
                "config": config_request_overrides_from_config(&config, ThreadParamsMode::Embedded)
            }
        });
        let thread_id = ThreadId::new();
        let saved_thread = json!({
            "id": thread_id, "sessionId": thread_id, "preview": "", "ephemeral": false,
            "modelProvider": "saved-provider", "createdAt": 0, "updatedAt": 0,
            "status": {"type": "idle"}, "cwd": config.cwd, "cliVersion": "test",
            "source": "cli", "turns": []
        });
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = tokio_tungstenite::accept_async(stream).await?;
            let mut catalog_requests = 0;
            let mut expected_provider = json!(null);
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let JSONRPCMessage::Request(request) = serde_json::from_str(&text)? else {
                    continue;
                };
                let mut reply = match request.method.as_str() {
                    "initialize" => json!({"result": {"userAgent": "catalog-test/1.0.0"}}),
                    "account/read" => {
                        // A remote draft previews the server's default, not the
                        // local provider or the server's globally active login.
                        assert_eq!(request.params.as_ref(), Some(&expected_account_params));
                        json!({"result": {"account": null, "requiresOpenaiAuth": false}})
                    }
                    "thread/read" => {
                        expected_provider = json!("saved-provider");
                        expected_account_params = json!({"threadId": thread_id, "newThread": null});
                        assert_eq!(request.params, Some(json!({"threadId": thread_id})));
                        json!({"result": {"thread": saved_thread}})
                    }
                    "model/list" => {
                        // Reattachment must select the saved provider before discovering models,
                        // independently of the client's invocation and server defaults.
                        let provider = request
                            .params
                            .as_ref()
                            .and_then(|params| params.get("modelProvider"));
                        assert_eq!(provider, Some(&expected_provider));
                        json!({"result": {"data": [], "nextCursor": null}})
                    }
                    "configRequirements/read" => json!({"result": {"requirements": null}}),
                    "collaborationMode/list" => {
                        assert_eq!(request.params, Some(json!({})));
                        catalog_requests += 1;
                        if catalog_requests == 1 {
                            let Some(reply) = initial_reply.clone() else {
                                continue;
                            };
                            reply
                        } else {
                            json!({"result": {"data": [{"name": "Server plan", "mode": "plan", "model": "server-model", "reasoning_effort": "high"}]}})
                        }
                    }
                    method => panic!("unexpected request: {method}"),
                };
                reply["id"] = json!(request.id);
                socket.send(Message::Text(reply.to_string().into())).await?;
            }
            Ok::<_, color_eyre::Report>(catalog_requests)
        });
        let mut session = AppServerSession::new(
            crate::backend_id::BackendId::new(),
            crate::connect_remote_app_server(endpoint).await?,
            ThreadParamsMode::Remote,
        );
        let initial = session.bootstrap(&config).await?;
        assert_eq!(
            (initial.default_model, initial.collaboration_modes),
            ("task-model".into(), vec![])
        );
        let refreshed = session
            .bootstrap_for_thread(&config, Some(thread_id))
            .await?;
        assert_eq!(
            refreshed.collaboration_modes,
            vec![CollaborationModeMask {
                name: "Server plan".into(),
                mode: Some(ModeKind::Plan),
                model: Some("server-model".into()),
                reasoning_effort: Some(Some(ReasoningEffort::High)),
                developer_instructions: Some(None),
            }]
        );
        session.shutdown().await?;
        assert_eq!(server.await??, 2);
    }
    Ok(())
}
