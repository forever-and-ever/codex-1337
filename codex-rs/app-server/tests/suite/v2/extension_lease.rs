use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadExtensionAttachResponse;
use codex_app_server_protocol::ThreadExtensionDetachResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadUnsubscribeResponse;
use core_test_support::skip_if_remote;
use core_test_support::stdio_server_bin;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

#[tokio::test]
async fn loaded_thread_extension_lease_is_idempotent_and_name_exclusive() -> Result<()> {
    skip_if_remote!(Ok(()), "the stdio MCP executable is host-local");
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_provider_config("supports_websockets = false")
        .write(codex_home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let thread_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let attach = json!({
        "threadId": thread_id,
        "serverName": "leased_mcp",
        "serverConfig": {
            "command": stdio_server_bin()?,
            "required": true,
            "startup_timeout_sec": 10,
        },
        "developerInstructions": "Use the leased MCP server while it is attached.",
    });
    let request = app
        .send_raw_request("thread/extensionAttach", Some(attach.clone()))
        .await?;
    let first: ThreadExtensionAttachResponse = app.read_response(request).await?;
    let request = app
        .send_raw_request("thread/extensionAttach", Some(attach.clone()))
        .await?;
    let repeated: ThreadExtensionAttachResponse = app.read_response(request).await?;
    assert_eq!(repeated, first);

    let mut conflicting = attach;
    conflicting["developerInstructions"] = json!("Different instructions");
    let request = app
        .send_raw_request("thread/extensionAttach", Some(conflicting))
        .await?;
    let error = app
        .read_stream_until_error_message(RequestId::Integer(request))
        .await?;
    assert_eq!(error.error.code, -32600);
    assert_eq!(error.error.message, "extension lease conflict");

    let detach = json!({"threadId": thread_id, "leaseId": first.lease_id});
    let request = app
        .send_raw_request("thread/extensionDetach", Some(detach.clone()))
        .await?;
    let _: ThreadExtensionDetachResponse = app.read_response(request).await?;
    let request = app
        .send_raw_request("thread/extensionDetach", Some(detach))
        .await?;
    let _: ThreadExtensionDetachResponse = app.read_response(request).await?;
    Ok(())
}

#[tokio::test]
async fn extension_cannot_replace_a_configured_server() -> Result<()> {
    skip_if_remote!(Ok(()), "the stdio MCP executable is host-local");
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_provider_config("supports_websockets = false")
        .with_extra_config(
            "[mcp_servers.leased_mcp]\ncommand = \"nonexistent-server\"\nenabled = false",
        )
        .write(codex_home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let thread_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let request = app
        .send_raw_request(
            "thread/extensionAttach",
            Some(json!({
                "threadId": thread_id,
                "serverName": "leased_mcp",
                "serverConfig": { "command": stdio_server_bin()? },
                "developerInstructions": "Temporary instructions",
            })),
        )
        .await?;
    let error = app
        .read_stream_until_error_message(RequestId::Integer(request))
        .await?;
    assert_eq!(error.error.code, -32600);
    assert!(error.error.message.contains("name is already registered"));
    Ok(())
}

// A client can unsubscribe before the attach response exists. The unsubscribe must revoke the
// pending lease before its serialized RPC gets a turn, or initialization can later resurrect it.
#[tokio::test]
async fn unsubscribe_revokes_pending_extension_attach() -> Result<()> {
    skip_if_remote!(Ok(()), "the stdio MCP executable is host-local");
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_provider_config("supports_websockets = false")
        .write(codex_home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let thread_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let barrier = codex_home.path().join("allow-mcp-initialize");
    let pid_file = codex_home.path().join("extension-mcp.pid");
    let attach = app
        .send_raw_request(
            "thread/extensionAttach",
            Some(json!({
                "threadId": thread_id,
                "serverName": "leased_mcp",
                "serverConfig": {
                    "command": stdio_server_bin()?,
                    "required": true,
                    "startup_timeout_sec": 30,
                    "env": {
                        "MCP_TEST_INITIALIZE_BARRIER_FILE": barrier,
                        "MCP_TEST_PID_FILE": pid_file,
                    },
                },
                "developerInstructions": "Use this temporary MCP lease.",
            })),
        )
        .await?;
    timeout(Duration::from_secs(10), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let unsubscribe = app
        .send_raw_request(
            "thread/unsubscribe",
            Some(json!({
                "threadId": thread_id,
            })),
        )
        .await?;
    let attach_error = timeout(
        Duration::from_secs(5),
        app.read_stream_until_error_message(RequestId::Integer(attach)),
    )
    .await??;
    assert_eq!(attach_error.error.code, -32600);
    assert!(
        attach_error
            .error
            .message
            .contains("extension MCP lease revoked")
    );
    let _: ThreadUnsubscribeResponse = app.read_response(unsubscribe).await?;
    Ok(())
}
