use super::GrpcCodeModeHost;
use super::GrpcStream;
use codex_code_mode_protocol::grpc as proto;
use codex_code_mode_protocol::grpc::code_mode_host_server::CodeModeHost;
use futures::FutureExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use tonic::Code;
use tonic::Request;

async fn assert_finished_event(
    events: &mut GrpcStream<proto::SessionEvent>,
    execution_id: &str,
    cell_id: &str,
) {
    assert_eq!(
        events.next().await.unwrap().unwrap(),
        proto::SessionEvent {
            event: Some(proto::session_event::Event::CellFinished(
                proto::CellFinished {
                    execution_id: execution_id.into(),
                    cell_id: cell_id.into(),
                    outcome: proto::cell_finished::Outcome::Completed as i32,
                }
            )),
        }
    );
}

#[tokio::test]
async fn pending_outcome_is_enqueued_before_callback_retirement() {
    let host = GrpcCodeModeHost::new();
    let (session_id, mut events) = open_session(&host).await;
    let session = host.state.session(&session_id).unwrap();
    session.reserve_execution("early").unwrap();
    // Pure scripts may commit and close before their admission handoff finishes.
    session.finish_cell("cell", codex_code_mode_protocol::CellOutcome::Completed);
    session.close_cell("cell");
    session
        .admit_execution(
            "early".into(),
            "cell".into(),
            host.state.cell_permit().unwrap(),
            /*traceparent*/ None,
        )
        .unwrap();
    assert_finished_event(&mut events, "early", "cell").await;
    assert_eq!(
        events.next().await.unwrap().unwrap(),
        proto::SessionEvent {
            event: Some(proto::session_event::Event::CellClosed(proto::CellClosed {
                execution_id: "early".into(),
                cell_id: "cell".into(),
                final_tool_call_sequence: 0,
            })),
        }
    );
}

pub(super) async fn open_session(
    host: &GrpcCodeModeHost,
) -> (String, GrpcStream<proto::SessionEvent>) {
    let mut stream = host
        .open_session(Request::new(proto::OpenSessionRequest {
            cell_execution_limits: None,
            output_truncation: None,
        }))
        .await
        .expect("open code-mode session")
        .into_inner();
    let event = stream
        .next()
        .await
        .expect("session opened event")
        .expect("session event");
    let Some(proto::session_event::Event::Opened(opened)) = event.event else {
        panic!("expected the first session event to open its lease");
    };
    (opened.session_id, stream)
}

pub(super) fn execute_request(
    session_id: &str,
    execution_id: &str,
    source: &str,
) -> proto::ExecuteRequest {
    proto::ExecuteRequest {
        session_id: session_id.to_string(),
        execution_id: execution_id.to_string(),
        tool_call_id: "outer-call".to_string(),
        source: source.to_string(),
        enabled_tools: Vec::new(),
        yield_time_ms: Some(1),
        max_output_tokens: None,
    }
}

pub(super) fn tool(name: &str) -> proto::ToolDefinition {
    proto::ToolDefinition {
        summary: String::new(),
        name: name.to_string(),
        tool_name: Some(proto::ToolName {
            name: name.to_string(),
            namespace: None,
        }),
        description: String::new(),
        kind: proto::ToolKind::Function as i32,
        input_schema_json: None,
        output_schema_json: None,
    }
}

pub(super) async fn execute_events(
    host: &GrpcCodeModeHost,
    request: proto::ExecuteRequest,
) -> (String, GrpcStream<proto::ExecuteEvent>) {
    let mut stream = host
        .execute(Request::new(request))
        .await
        .expect("execute cell")
        .into_inner();
    let event = stream
        .next()
        .await
        .expect("execution started event")
        .expect("execution event");
    let Some(proto::execute_event::Event::Started(started)) = event.event else {
        panic!("expected execution admission before its outcome");
    };
    (started.cell_id, stream)
}

#[tokio::test]
async fn execute_stream_starts_immediately_and_wait_preserves_missing_cells() {
    let host = GrpcCodeModeHost::new();
    let (session_id, mut session_events) = open_session(&host).await;
    let mut request = execute_request(
        &session_id,
        "execution-1",
        r#"text("before"); yield_control(); text("after");"#,
    );
    request.yield_time_ms = Some(60_000);
    let (cell_id, mut execution) = execute_events(&host, request).await;

    let yielded = execution.next().await.unwrap().unwrap();
    let Some(proto::execute_event::Event::Outcome(yielded_outcome)) = &yielded.event else {
        panic!("expected execution outcome");
    };
    assert_eq!(
        yielded,
        proto::ExecuteEvent {
            event: Some(proto::execute_event::Event::Outcome(
                proto::ExecutionOutcome {
                    code_mode_host_duration_ns: yielded_outcome.code_mode_host_duration_ns,
                    cell_id: cell_id.clone(),
                    content_items: vec![proto::ContentItem {
                        item: Some(proto::content_item::Item::Text(proto::TextContent {
                            max_tokens: None,
                            text: "before".to_string(),
                        })),
                    }],
                    outcome: Some(proto::execution_outcome::Outcome::Yielded(
                        proto::ExecutionYielded {},
                    )),
                }
            )),
        }
    );
    let completed = host
        .wait(Request::new(proto::WaitRequest {
            session_id: session_id.clone(),
            cell_id: cell_id.clone(),
            wait_id: "wait-1".to_string(),
            yield_time_ms: 60_000,
            max_output_tokens: None,
        }))
        .await
        .expect("wait for completion")
        .into_inner();
    let Some(proto::wait_response::State::LiveCell(completed_outcome)) = &completed.state else {
        panic!("expected live-cell wait outcome");
    };
    assert_eq!(
        completed,
        proto::WaitResponse {
            state: Some(proto::wait_response::State::LiveCell(
                proto::ExecutionOutcome {
                    code_mode_host_duration_ns: completed_outcome.code_mode_host_duration_ns,
                    cell_id: cell_id.clone(),
                    content_items: vec![proto::ContentItem {
                        item: Some(proto::content_item::Item::Text(proto::TextContent {
                            max_tokens: None,
                            text: "after".to_string(),
                        })),
                    }],
                    outcome: Some(proto::execution_outcome::Outcome::Completed(
                        proto::ExecutionCompleted { error_text: None },
                    )),
                }
            )),
        }
    );
    assert_finished_event(&mut session_events, "execution-1", &cell_id).await;
    assert_eq!(
        session_events.next().await.unwrap().unwrap(),
        proto::SessionEvent {
            event: Some(proto::session_event::Event::CellClosed(proto::CellClosed {
                execution_id: "execution-1".to_string(),
                cell_id: cell_id.clone(),
                final_tool_call_sequence: 0,
            })),
        }
    );

    let missing = host
        .wait(Request::new(proto::WaitRequest {
            session_id,
            cell_id: "missing-cell".to_string(),
            wait_id: "wait-missing".to_string(),
            yield_time_ms: 1,
            max_output_tokens: None,
        }))
        .await
        .expect("missing cells remain successful wait outcomes")
        .into_inner();
    assert!(matches!(
        missing.state,
        Some(proto::wait_response::State::MissingCell(_))
    ));
}

/// Missing, empty, and explicit default namespaces identify the same subscribed tool.
#[tokio::test]
async fn filtered_subscriptions_match_default_namespace_aliases() {
    let aliases = [
        (None, Some("functions".to_string())),
        (Some(String::new()), None),
        (Some("functions".to_string()), Some(String::new())),
    ];

    for (tool_namespace, filter_namespace) in aliases {
        let host = GrpcCodeModeHost::new();
        let (session_id, _events) = open_session(&host).await;
        let mut subscription = host
            .subscribe_to_tool_calls(Request::new(proto::SubscribeToToolCallsRequest {
                session_id: session_id.clone(),
                tool_names: vec![proto::ToolName {
                    name: "echo".to_string(),
                    namespace: filter_namespace,
                }],
            }))
            .await
            .expect("subscribe with a default-namespace alias")
            .into_inner();
        let mut request = execute_request(
            &session_id,
            "default-namespace",
            "text(await tools.echo({}));",
        );
        request.yield_time_ms = Some(/*value*/ 60_000);
        let mut enabled_tool = tool("echo");
        enabled_tool.tool_name = Some(proto::ToolName {
            name: "echo".to_string(),
            namespace: tool_namespace.clone(),
        });
        request.enabled_tools = vec![enabled_tool];
        let (_cell_id, mut execution) = execute_events(&host, request).await;

        let invocation = subscription
            .next()
            .await
            .expect("matching default-namespace invocation")
            .expect("tool invocation");
        assert_eq!(
            invocation.tool_name,
            Some(proto::ToolName {
                name: "echo".to_string(),
                namespace: tool_namespace,
            })
        );
        host.complete_tool_call(Request::new(proto::CompleteToolCallRequest {
            session_id,
            invocation_id: invocation.invocation_id,
            outcome: Some(proto::complete_tool_call_request::Outcome::Succeeded(
                proto::ToolCallSucceeded {
                    output_json: br#""done""#.to_vec(),
                },
            )),
        }))
        .await
        .expect("complete the matching tool call");
        assert!(matches!(
            execution.next().await,
            Some(Ok(proto::ExecuteEvent {
                event: Some(proto::execute_event::Event::Outcome(
                    proto::ExecutionOutcome {
                        outcome: Some(proto::execution_outcome::Outcome::Completed(_)),
                        ..
                    }
                )),
            }))
        ));
    }
}

#[tokio::test]
async fn filtered_subscriptions_receive_ordered_calls_and_unary_completions() {
    let host = GrpcCodeModeHost::new();
    let (session_id, mut session_events) = open_session(&host).await;
    let mut matching = host
        .subscribe_to_tool_calls(Request::new(proto::SubscribeToToolCallsRequest {
            session_id: session_id.clone(),
            tool_names: vec![proto::ToolName {
                name: "echo".to_string(),
                namespace: None,
            }],
        }))
        .await
        .unwrap()
        .into_inner();
    let mut unrelated = host
        .subscribe_to_tool_calls(Request::new(proto::SubscribeToToolCallsRequest {
            session_id: session_id.clone(),
            tool_names: vec![proto::ToolName {
                name: "other".to_string(),
                namespace: None,
            }],
        }))
        .await
        .unwrap()
        .into_inner();
    let mut request = execute_request(
        &session_id,
        "execution-tools",
        r#"text(await tools.echo({value: 1})); text(await tools.echo({value: 2}));"#,
    );
    request.yield_time_ms = Some(60_000);
    request.enabled_tools = vec![tool("echo")];
    let (cell_id, mut execution) = execute_events(&host, request).await;

    for sequence in [1, 2] {
        let invocation = matching.next().await.unwrap().unwrap();
        assert_eq!(invocation.session_id, session_id);
        assert_eq!(invocation.execution_id, "execution-tools");
        assert_eq!(invocation.cell_id, cell_id);
        assert_eq!(invocation.sequence, sequence);
        assert_eq!(
            invocation.input_json,
            Some(format!(r#"{{"value":{sequence}}}"#).into_bytes())
        );
        assert!(unrelated.next().now_or_never().is_none());
        host.complete_tool_call(Request::new(proto::CompleteToolCallRequest {
            session_id: session_id.clone(),
            invocation_id: invocation.invocation_id,
            outcome: Some(proto::complete_tool_call_request::Outcome::Succeeded(
                proto::ToolCallSucceeded {
                    output_json: format!(r#""result-{sequence}""#).into_bytes(),
                },
            )),
        }))
        .await
        .expect("complete delegated tool");
    }

    let outcome = execution.next().await.unwrap().unwrap();
    assert!(matches!(
        outcome.event,
        Some(proto::execute_event::Event::Outcome(
            proto::ExecutionOutcome {
                outcome: Some(proto::execution_outcome::Outcome::Completed(_)),
                ..
            }
        ))
    ));
    assert_finished_event(&mut session_events, "execution-tools", &cell_id).await;
    assert_eq!(
        session_events.next().await.unwrap().unwrap(),
        proto::SessionEvent {
            event: Some(proto::session_event::Event::CellClosed(proto::CellClosed {
                execution_id: "execution-tools".to_string(),
                cell_id,
                final_tool_call_sequence: 2,
            })),
        }
    );
}

#[tokio::test]
async fn cancellation_before_wait_admission_is_preserved() {
    let host = GrpcCodeModeHost::new();
    let (session_id, _events) = open_session(&host).await;
    let (cell_id, mut execution) = execute_events(
        &host,
        execute_request(
            &session_id,
            "execution-wait",
            "await new Promise(() => {});",
        ),
    )
    .await;
    execution.next().await.unwrap().unwrap();

    host.cancel_wait(Request::new(proto::CancelWaitRequest {
        session_id: session_id.clone(),
        wait_id: "pre-cancelled".to_string(),
    }))
    .await
    .expect("record cancellation before wait admission");
    let cancelled = host
        .wait(Request::new(proto::WaitRequest {
            session_id: session_id.clone(),
            cell_id: cell_id.clone(),
            wait_id: "pre-cancelled".to_string(),
            yield_time_ms: 60_000,
            max_output_tokens: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(cancelled.code(), Code::Cancelled);

    host.terminate(Request::new(proto::TerminateRequest {
        session_id,
        cell_id,
        max_output_tokens: None,
    }))
    .await
    .unwrap();
}

#[tokio::test]
async fn yield_before_observation_registration_is_remembered() {
    let host = GrpcCodeModeHost::new();
    let (session_id, _events) = open_session(&host).await;
    let session = host.state.session(&session_id).unwrap();
    for observation in [
        proto::yield_observation_request::Observation::ExecutionId("exec".to_string()),
        proto::yield_observation_request::Observation::WaitId("wait".to_string()),
    ] {
        host.yield_observation(Request::new(proto::YieldObservationRequest {
            session_id: session_id.clone(),
            observation: Some(observation),
        }))
        .await
        .unwrap();
    }

    assert!(session.reserve_execution("exec").unwrap().is_cancelled());
    let wait = super::waits::WaitRegistration::new(session, "wait".to_string()).unwrap();
    assert!(wait.yield_signal().is_cancelled());
}

#[tokio::test]
async fn notifications_do_not_delay_cell_completion() {
    let host = GrpcCodeModeHost::new();
    let (session_id, mut session_events) = open_session(&host).await;
    let mut request = execute_request(
        &session_id,
        "execution-notify",
        r#"notify("pending"); text("done");"#,
    );
    request.yield_time_ms = Some(60_000);
    let (cell_id, mut execution) = execute_events(&host, request).await;
    let event = session_events.next().await.unwrap().unwrap();
    let Some(proto::session_event::Event::Notification(notification)) = event.event else {
        panic!("expected pending notification");
    };
    assert_eq!(notification.execution_id, "execution-notify");
    assert_eq!(notification.cell_id, cell_id);
    assert_eq!(notification.call_id, "outer-call");
    assert_eq!(notification.text, "pending");
    assert!(matches!(
        execution.next().await.unwrap().unwrap().event,
        Some(proto::execute_event::Event::Outcome(
            proto::ExecutionOutcome {
                outcome: Some(proto::execution_outcome::Outcome::Completed(_)),
                ..
            }
        ))
    ));
    assert_finished_event(&mut session_events, "execution-notify", &cell_id).await;
    assert_eq!(
        session_events.next().await.unwrap().unwrap(),
        proto::SessionEvent {
            event: Some(proto::session_event::Event::CellClosed(proto::CellClosed {
                execution_id: "execution-notify".to_string(),
                cell_id,
                final_tool_call_sequence: 0,
            })),
        }
    );
    host.acknowledge_notification(Request::new(proto::AcknowledgeNotificationRequest {
        session_id,
        notification_id: notification.notification_id,
    }))
    .await
    .expect("legacy notification acknowledgments remain accepted");
}
