//! Verifies that the agent retries when the SSE stream terminates before
//! delivering a `response.completed` event.

use codex_core::TurnInputRequest;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::mock_server;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::net::TcpListener;
use wiremock::MockServer;

fn sse_incomplete() -> String {
    // A later start and an earlier completion must not steal an open item's deltas.
    responses::sse(vec![
        responses::ev_message_item_added("committed", ""),
        serde_json::json!({"type":"response.output_text.delta","item_id":"committed","delta":"Retain "}),
        responses::ev_reasoning_item_added("unfinished", &[]),
        serde_json::json!({"type":"response.reasoning_summary_text.delta","item_id":"unfinished","summary_index":0,"delta":"Discard this "}),
        serde_json::json!({"type":"response.output_text.delta","item_id":"committed","delta":"this completed message."}),
        responses::ev_assistant_message("committed", "Retain this completed message."),
        serde_json::json!({"type":"response.reasoning_summary_text.delta","item_id":"unfinished","summary_index":0,"delta":"unfinished text."}),
        responses::ev_message_item_added("unfinished-next", ""),
        serde_json::json!({"type":"response.output_text.delta","item_id":"unfinished-next","delta":"Discard this overlapping item too."}),
        serde_json::json!({"type":"response.reasoning_summary_text.delta","item_id":"unfinished","summary_index":0,"delta":" Still open."}),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_on_early_close() {
    skip_if_no_network!();

    let incomplete_sse = sse_incomplete();
    let completed_sse = responses::sse_completed("resp_ok");

    let (server, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: incomplete_sse,
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: completed_sse,
        }],
    ])
    .await;

    // Configure retry behavior explicitly to avoid mutating process-wide
    // environment variables.

    let model_provider = ModelProviderInfo {
        name: "openai".to_owned(),
        adapter: Default::default(),
        base_url: Some(format!("{}/v1", server.uri())),
        model_catalog_url: None,
        // Environment variable that should exist in the test environment.
        // ModelClient will return an error if the environment variable for the
        // provider is not set.
        env_key: Some("PATH".to_owned()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        gateway_oauth: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        // exercise retry path: first attempt yields incomplete stream, so allow 1 retry
        request_max_retries: Some(0),
        stream_max_retries: Some(1),
        stream_idle_timeout_ms: Some(2000),
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_standalone_web_search: false,
        capabilities: None,
        include_internal_metadata: false,
    };

    let TestCodex { codex, .. } = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
        })
        .build_with_streaming_server(&server)
        .await
        .unwrap();

    codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".to_owned(),
            text_elements: Vec::new(),
        }]))
        .await
        .unwrap();

    // The retry's semantic boundary must not depend on an optional status warning.
    let mut discarded = Vec::new();
    let mut text_deltas = Vec::new();
    let mut reasoning_deltas = Vec::new();
    wait_for_event(&codex, |event| {
        match event {
            EventMsg::AgentMessageContentDelta(event) => {
                text_deltas.push((event.item_id.clone(), event.delta.clone()))
            }
            EventMsg::ReasoningContentDelta(event) => {
                reasoning_deltas.push((event.item_id.clone(), event.delta.clone()))
            }
            _ => {}
        }
        if let EventMsg::StreamItemsDiscarded(event) = event {
            discarded.extend(event.item_ids.clone());
        }
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(discarded, ["unfinished", "unfinished-next"]);
    assert_eq!(
        text_deltas,
        [
            ("committed".to_owned(), "Retain ".to_owned()),
            ("committed".to_owned(), "this completed message.".to_owned()),
            (
                "unfinished-next".to_owned(),
                "Discard this overlapping item too.".to_owned()
            ),
        ]
    );
    assert_eq!(
        reasoning_deltas,
        [
            ("unfinished".to_owned(), "Discard this ".to_owned()),
            ("unfinished".to_owned(), "unfinished text.".to_owned()),
            ("unfinished".to_owned(), " Still open.".to_owned()),
        ]
    );

    let requests = server.requests().await;
    assert_eq!(
        requests.len(),
        2,
        "expected retry after incomplete SSE stream"
    );

    let retried: serde_json::Value = serde_json::from_slice(&requests[1]).unwrap();
    let input = retried["input"].to_string();
    assert!(input.contains("Retain this completed message."));
    assert!(!input.contains("Discard this unfinished text."));
    assert!(!input.contains("Discard this overlapping item too."));

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_failure_pauses_retry_budget_until_provider_is_reachable() -> anyhow::Result<()>
{
    skip_if_no_network!(Ok(()));

    let bootstrap_server = responses::start_mock_server().await;
    let unavailable_listener = TcpListener::bind("127.0.0.1:0")?;
    let unavailable_address = unavailable_listener.local_addr()?;
    drop(unavailable_listener);

    let TestCodex { codex, .. } = test_codex()
        .with_config(move |config| {
            config.model_provider.base_url = Some(format!("http://{unavailable_address}/v1"));
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
            config.model_provider.supports_websockets = false;
        })
        .build_with_auto_env(&bootstrap_server)
        .await?;

    codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "recover after the network returns".into(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let EventMsg::StreamError(connection_error) =
        wait_for_event(&codex, |event| matches!(event, EventMsg::StreamError(_))).await
    else {
        unreachable!("predicate guarantees a stream error event");
    };
    assert_eq!(
        connection_error.message,
        "Reconnecting... waiting for network"
    );

    let recovered_server = MockServer::builder()
        .listener(TcpListener::bind(unavailable_address)?)
        .start()
        .await;
    mock_server::run(&[&recovered_server], async {
        let response_mock = responses::mount_sse_sequence(
            &recovered_server,
            vec![sse_incomplete(), responses::sse_completed("resp_recovered")],
        )
        .await;

        let EventMsg::StreamError(stream_error) =
            wait_for_event(&codex, |event| matches!(event, EventMsg::StreamError(_))).await
        else {
            unreachable!("predicate guarantees a stream error event");
        };
        assert_eq!(stream_error.message, "Reconnecting... 1/1");

        let EventMsg::TurnComplete(completed) =
            wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await
        else {
            unreachable!("predicate guarantees a turn complete event");
        };

        assert_eq!(completed.error, None);
        assert_eq!(response_mock.requests().len(), 2);

        Ok(())
    })
    .await
}
