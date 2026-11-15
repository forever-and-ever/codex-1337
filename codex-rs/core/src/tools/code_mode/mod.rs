mod delegate;
mod disclosure;
mod lifecycle;

pub(crate) use lifecycle::CaptureOrigin;
pub(crate) use lifecycle::NotificationLease;
pub(crate) use lifecycle::ProcessWait;
mod execute_handler;
pub(crate) mod execute_spec;
mod output;
mod output_sections;
mod response_adapter;
mod telemetry;
mod tool_definitions;
pub(crate) mod transcript;
mod wait_handler;
pub(crate) mod wait_spec;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_code_mode::CellId;
use codex_code_mode::CodeModeNestedToolCall;
use codex_code_mode::CodeModeOutputTruncation;
use codex_code_mode::CodeModeSession;
use codex_code_mode::CodeModeSessionProvider;
use codex_code_mode::CodeModeToolKind;
use codex_code_mode::RuntimeResponse;
use codex_protocol::ThreadId;
use futures::future::join_all;
use serde_json::Value as JsonValue;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::function_tool::FunctionCallError;
use crate::original_image_detail::can_request_original_image_detail;
use crate::original_image_detail::sanitize_original_image_detail as sanitize_image_detail_items;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use crate::tools::ExecutedToolCalls;
use crate::tools::call_trace;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::router::ToolCall;
use crate::tools::router::ToolCallSource;
use codex_protocol::openai_models::ToolMode;
use codex_tools::ToolName;

use delegate::CodeModeCellDelegate;
use delegate::CodeModeDispatchBroker;
use delegate::CodeModeDispatchWorker;
pub(crate) use disclosure::Disclosure;
pub(crate) use execute_handler::CodeModeExecuteHandler;
use output::CodeModeToolOutput;
use response_adapter::into_function_call_output_content_items;
pub(crate) use tool_definitions::prepare_code_mode_tool_definitions;
pub(crate) use wait_handler::CodeModeWaitHandler;

pub(crate) const PUBLIC_TOOL_NAME: &str = codex_code_mode::PUBLIC_TOOL_NAME;
pub(crate) const WAIT_TOOL_NAME: &str = codex_code_mode::WAIT_TOOL_NAME;

/// Returns true for the code-mode `exec` tool in the default namespace.
pub(crate) fn is_exec_tool_name(tool_name: &ToolName) -> bool {
    tool_name.is_default_namespace() && tool_name.name == PUBLIC_TOOL_NAME
}

#[derive(Clone)]
pub(crate) struct ExecContext {
    pub(super) session: Arc<Session>,
    pub(super) turn: Arc<TurnContext>,
}

pub(crate) struct CodeModeService {
    transcript: Arc<lifecycle::TranscriptRegistry>,
    session: OnceCell<Arc<dyn CodeModeSession>>,
    session_provider: Arc<dyn CodeModeSessionProvider>,
    availability: Result<(), String>,
    dispatch_broker: Arc<CodeModeDispatchBroker>,
    shutdown_token: CancellationToken,
    unavailable_warning_emitted: AtomicBool,
    output_retention: output_sections::OutputRetention,
}

impl CodeModeService {
    pub(crate) fn new(
        thread_id: ThreadId,
        session_provider: Arc<dyn CodeModeSessionProvider>,
        executed_tool_calls: ExecutedToolCalls,
    ) -> Self {
        let dispatch_broker = Arc::new(CodeModeDispatchBroker::new(thread_id, executed_tool_calls));
        let availability = session_provider.availability();
        Self {
            transcript: Arc::default(),
            session: OnceCell::new(),
            session_provider,
            availability,
            dispatch_broker,
            shutdown_token: CancellationToken::new(),
            unavailable_warning_emitted: AtomicBool::new(false),
            output_retention: output_sections::OutputRetention::default(),
        }
    }

    pub(crate) fn is_available(&self) -> bool {
        self.availability.is_ok()
    }

    pub(crate) fn take_unavailable_warning(&self, tool_mode: ToolMode) -> Option<String> {
        let error = self.availability.as_ref().err()?;
        let behavior = match tool_mode {
            ToolMode::Direct => "Falling back to direct tools",
            ToolMode::CodeMode | ToolMode::CodeModeOnly => "Code mode will fail closed",
        };
        (!self
            .unavailable_warning_emitted
            .swap(true, Ordering::Relaxed))
        .then(|| {
            format!(
                "Code Mode is unavailable because {error}. {behavior}; enable `features.code_mode_host` and install `codex-code-mode-host`."
            )
        })
    }

    pub(crate) fn session_provider(&self) -> Arc<dyn CodeModeSessionProvider> {
        Arc::clone(&self.session_provider)
    }

    fn output_truncation(&self) -> CodeModeOutputTruncation {
        self.session
            .get()
            .map(|session| session.output_truncation())
            .unwrap_or_default()
    }

    async fn execute(
        &self,
        request: codex_code_mode::ExecuteRequest,
        script: String,
        step_context: Arc<StepContext>,
        session: std::sync::Weak<Session>,
    ) -> Result<(codex_code_mode::StartedCell, Arc<lifecycle::ExecutionScope>), String> {
        let news = step_context.interjection.news().clone();
        let exec_call_id = request.tool_call_id.clone();
        let starting = self
            .transcript
            .open(
                session.clone(),
                Arc::clone(&step_context.turn),
                exec_call_id,
                script,
                self.shutdown_token.clone(),
            )
            .await?;
        let delegate = Arc::new(CodeModeCellDelegate {
            custody: std::sync::Mutex::new(lifecycle::CellCustody::Executing {
                _hold: starting.scope().execution_hold(),
            }),
            broker: Arc::clone(&self.dispatch_broker),
            step_context,
            scope: starting.scope(),
            fence: crate::tools::fence::Prerequisite::current(),
        });
        let result = match self.session().await {
            Ok(runtime) => runtime.execute(request, delegate, Some(news)).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(started) => {
                let scope = starting.scope();
                starting.allocated(&started.cell_id).await;
                Ok((started, scope))
            }
            Err(error) => {
                starting.failed().await;
                Err(error)
            }
        }
    }

    fn transcript_scope(&self, cell_id: &CellId) -> Option<Arc<lifecycle::ExecutionScope>> {
        self.transcript.scope(cell_id)
    }

    pub(crate) fn transcript_owner(
        &self,
        call: &str,
    ) -> Option<codex_protocol::code_mode::CodeModeOwner> {
        self.transcript.owner(call)
    }

    pub(crate) fn wrap_transcript_event(
        &self,
        event: codex_protocol::protocol::EventMsg,
        owner: Option<codex_protocol::code_mode::CodeModeOwner>,
    ) -> codex_protocol::protocol::EventMsg {
        self.transcript.wrap(event, owner)
    }

    pub(crate) fn release_transcript_child(&self, call: &str) {
        self.transcript.release_child(call);
    }

    pub(crate) fn observe_transcript_child(&self, event: &codex_protocol::protocol::EventMsg) {
        self.transcript.observe_child(event);
    }

    pub(crate) fn release_transcript_capture(&self, call: &str, origin: CaptureOrigin) {
        self.transcript.release_capture(call, origin);
    }

    pub(crate) fn begin_notification_capture(
        &self,
        owner: codex_protocol::code_mode::CodeModeOwner,
        call: String,
        queue: std::sync::Weak<tokio::sync::Mutex<crate::state::TurnState>>,
    ) -> NotificationLease {
        self.transcript.notification(&owner, call, queue)
    }

    pub(crate) async fn wait(
        &self,
        request: codex_code_mode::WaitRequest,
        news: CancellationToken,
    ) -> Result<codex_code_mode::WaitOutcome, String> {
        self.session().await?.wait(request, Some(news)).await
    }

    pub(crate) async fn terminate(
        &self,
        request: codex_code_mode::TerminateRequest,
    ) -> Result<codex_code_mode::WaitOutcome, String> {
        self.session().await?.terminate(request).await
    }

    pub(crate) async fn interrupt_active_cells(&self) {
        let Some(session) = self.session.get() else {
            return;
        };
        join_all(
            self.dispatch_broker
                .active_cell_ids()
                .into_iter()
                .map(|cell_id| async move {
                    if let Err(error) = session.terminate(cell_id.clone().into()).await {
                        tracing::warn!(%cell_id, %error, "failed to terminate interrupted code-mode cell");
                    }
                }),
        )
        .await;
    }

    pub(crate) fn begin_shutdown(&self) {
        self.shutdown_token.cancel();
    }

    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        self.begin_shutdown();
        // Join any initialization already in progress without initializing an unused service.
        let result = match self
            .session
            .get_or_try_init(|| async {
                Err::<Arc<dyn CodeModeSession>, String>(
                    "code mode session is shutting down".to_string(),
                )
            })
            .await
        {
            Ok(session) => session.shutdown().await,
            Err(_) => Ok(()),
        };
        self.transcript.shutdown().await;
        self.output_retention.clear();
        result
    }

    pub(crate) fn mark_cell_ready_for_dispatch(
        &self,
        cell_id: &codex_code_mode::CellId,
        originating_call: Option<crate::tools::context::ToolCallOrigin>,
    ) {
        self.dispatch_broker
            .mark_cell_ready_for_dispatch(cell_id, originating_call);
    }

    pub(crate) fn cell_originating_call(
        &self,
        cell_id: &codex_code_mode::CellId,
    ) -> Option<crate::tools::context::ToolCallOrigin> {
        self.dispatch_broker.cell_originating_call(cell_id)
    }

    pub(crate) fn finish_cell_dispatch(&self, cell_id: &CellId) {
        self.dispatch_broker.close_cell(cell_id);
    }

    pub(crate) fn start_turn_worker(
        &self,
        session: &Arc<Session>,
        step_context: Arc<StepContext>,
        tool_runtime: ToolCallRuntime,
    ) -> Option<CodeModeDispatchWorker> {
        if !step_context.tool_router.requires_code_mode_worker() {
            return None;
        }

        Some(
            self.dispatch_broker
                .start_turn_worker(Arc::clone(session), tool_runtime),
        )
    }

    pub(crate) async fn session(&self) -> Result<Arc<dyn CodeModeSession>, String> {
        if self.shutdown_token.is_cancelled() {
            return Err("code mode session is shutting down".to_string());
        }
        self.session
            .get_or_try_init(|| async {
                if self.shutdown_token.is_cancelled() {
                    return Err("code mode session is shutting down".to_string());
                }
                let session = tokio::select! {
                    biased;
                    _ = self.shutdown_token.cancelled() => {
                        return Err("code mode session is shutting down".to_string());
                    }
                    session = self
                        .session_provider
                        .create_session() => session?,
                };
                if self.shutdown_token.is_cancelled() {
                    let _ = session.shutdown().await;
                    return Err("code mode session is shutting down".to_string());
                }
                Ok(session)
            })
            .await
            .map(Arc::clone)
    }
}

fn handle_runtime_response(
    model_info: &codex_protocol::openai_models::ModelInfo,
    response: RuntimeResponse,
    max_output_tokens: Option<usize>,
    retention: &output_sections::OutputRetention,
    output_truncation: CodeModeOutputTruncation,
    wall_time: Duration,
    experimental_show_cell_overhead: bool,
) -> CodeModeToolOutput {
    if matches!(&response, RuntimeResponse::Yielded { .. }) {
        crate::tools::fence::Prerequisite::detached();
    }
    if matches!(&response, RuntimeResponse::Terminated { .. }) {
        let _ = crate::tools::fence::Prerequisite::current().finish(/*success*/ false);
    }
    let script_status = format_script_status(&response);
    let supports_original = can_request_original_image_detail(model_info);
    let host_duration = response.code_mode_host_duration();

    let (mut content_items, error_text) = match response {
        RuntimeResponse::Yielded { content_items, .. }
        | RuntimeResponse::Terminated { content_items, .. } => (content_items, None),
        RuntimeResponse::Result {
            content_items,
            error_text,
            ..
        } => (content_items, error_text),
    };
    let success = error_text.is_none();
    // TELOS(recoverable-output): client-side limits retain bounded sections and recovery.
    if output_truncation == CodeModeOutputTruncation::Client {
        if let Some(error_text) = error_text {
            content_items.push(codex_code_mode::FunctionCallOutputContentItem::InputText {
                text: format!("Script error:\n{error_text}"),
                max_tokens: None,
            });
        }
        content_items = output_sections::truncate_sections(
            content_items,
            max_output_tokens,
            model_info.truncation_policy.into(),
            retention,
        );
    }
    let mut content_items = into_function_call_output_content_items(content_items);
    sanitize_image_detail_items(supports_original, &mut content_items);
    CodeModeToolOutput::new(
        FunctionToolOutput::from_content(content_items, Some(success)),
        script_status,
        wall_time,
        host_duration,
        if experimental_show_cell_overhead {
            codex_protocol::ExecutionOutputMetadata::Included
        } else {
            codex_protocol::ExecutionOutputMetadata::Hidden
        },
    )
}

fn format_script_status(response: &RuntimeResponse) -> String {
    match response {
        RuntimeResponse::Yielded { cell_id, .. } => {
            format!("Script running with cell ID {cell_id}")
        }
        RuntimeResponse::Terminated { .. } => "Script terminated".to_string(),
        RuntimeResponse::Result { error_text, .. } => {
            if error_text.is_none() {
                "Script completed".to_string()
            } else {
                "Script failed".to_string()
            }
        }
    }
}

// Submit synchronously so the recorder sees the call before the cell's dispatch gate closes.
fn submit_nested_tool(
    session: Arc<Session>,
    step_context: Arc<StepContext>,
    tool_runtime: ToolCallRuntime,
    invocation: CodeModeNestedToolCall,
    call_id: String,
    cancellation_token: CancellationToken,
) -> Result<
    impl std::future::Future<Output = Result<JsonValue, FunctionCallError>> + Send + 'static,
    FunctionCallError,
> {
    let dispatch_started = std::time::Instant::now();
    let CodeModeNestedToolCall {
        cell_id,
        runtime_tool_call_id,
        tool_name,
        tool_kind,
        input,
    } = invocation;
    let thread_id = session.thread_id;
    let turn_id = step_context.turn.sub_id.clone();
    let tool_name = tool_name.with_default_namespace();
    // A cell can outlive a turn; the broker records arrival before a dispatching turn is known.
    tracing::event!(
        name: "codex.code_mode.nested_tool_dispatched",
        target: "codex_otel.trace_safe",
        tracing::Level::INFO,
        event.name = "codex.code_mode.nested_tool_dispatched",
        tool_name = tool_name.name.as_str(),
        tool_namespace = call_trace::namespace(&tool_name),
        tool_source = "code_mode",
        conversation.id = %thread_id,
        turn_id = turn_id.as_str(),
        cell.id = telemetry::trace_id(cell_id.as_str()),
        runtime_tool_call_id = telemetry::trace_id(&runtime_tool_call_id),
        call_id = call_id.as_str(),
    );
    let payload = if is_exec_tool_name(&tool_name) {
        Err(format!("{PUBLIC_TOOL_NAME} cannot invoke itself"))
    } else {
        build_nested_tool_payload(tool_kind, &tool_name, input)
    };
    let payload = match payload {
        Ok(payload) => payload,
        Err(error) => {
            call_trace::result_ready(
                thread_id,
                &turn_id,
                &tool_name,
                &call_id,
                call_trace::Source::CodeMode,
                dispatch_started.elapsed(),
            );
            return Err(FunctionCallError::RespondToModel(error));
        }
    };

    let call = ToolCall {
        tool_name,
        call_id,
        payload,
        encrypted_function_args: None,
        native_call: None,
    };
    session
        .services
        .analytics_events_client
        .track_code_mode_tool_call(codex_analytics::CodeModeToolCallFact::ChildStarted {
            thread_id: session.thread_id.to_string(),
            turn_id: step_context.turn.sub_id.clone(),
            call_id: call.call_id.clone(),
            cell_id: cell_id.to_string(),
        });
    let result = tool_runtime.handle_tool_call_with_source(
        step_context,
        call,
        ToolCallSource::CodeMode {
            cell_id: cell_id.to_string(),
            runtime_tool_call_id,
        },
        cancellation_token,
        Arc::default(),
        /*admission*/ None,
    );
    Ok(async move { Ok(result.await?.code_mode_result()) })
}

fn build_nested_tool_payload(
    tool_kind: CodeModeToolKind,
    tool_name: &ToolName,
    input: Option<JsonValue>,
) -> Result<ToolPayload, String> {
    match tool_kind {
        CodeModeToolKind::Function => build_function_tool_payload(tool_name, input),
        CodeModeToolKind::Freeform => build_freeform_tool_payload(tool_name, input),
    }
}

fn build_function_tool_payload(
    tool_name: &ToolName,
    input: Option<JsonValue>,
) -> Result<ToolPayload, String> {
    let arguments = serialize_function_tool_arguments(tool_name, input)?;
    Ok(ToolPayload::Function { arguments })
}

fn serialize_function_tool_arguments(
    tool_name: &ToolName,
    input: Option<JsonValue>,
) -> Result<String, String> {
    match input {
        None => Ok("{}".to_string()),
        Some(JsonValue::Object(map)) => serde_json::to_string(&JsonValue::Object(map))
            .map_err(|err| format!("failed to serialize tool `{tool_name}` arguments: {err}")),
        Some(_) => Err(format!(
            "tool `{tool_name}` expects a JSON object for arguments"
        )),
    }
}

fn build_freeform_tool_payload(
    tool_name: &ToolName,
    input: Option<JsonValue>,
) -> Result<ToolPayload, String> {
    match input {
        Some(JsonValue::String(input)) => Ok(ToolPayload::Custom { input }),
        _ => Err(format!("tool `{tool_name}` expects a string input")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::build_nested_tool_payload;
    use crate::session::step_context::StepContext;
    use crate::session::tests::make_session_and_context;
    use crate::tools::context::ToolPayload;
    use crate::tools::parallel::ToolCallRuntime;
    use crate::tools::registry::ToolRegistry;
    use crate::tools::router::ToolRouter;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_code_mode::CodeModeToolKind;
    use codex_protocol::openai_models::ToolMode;
    use codex_tools::ToolName;
    use serde_json::json;

    #[tokio::test]
    async fn turn_worker_uses_step_router_mode_instead_of_admitted_turn() {
        let (session, turn) = make_session_and_context().await;
        assert_eq!(
            crate::tools::effective_tool_mode(&turn, turn.model_info()),
            ToolMode::Direct
        );
        let session = Arc::new(session);
        let step_context = StepContext::for_test(Arc::new(turn));
        let router = Arc::new(ToolRouter::from_parts(
            ToolRegistry::empty_for_test(),
            Vec::new(),
            ToolMode::CodeModeOnly,
            BTreeMap::new(),
            /*exec_definitions*/ None,
            /*tool_namespaces_info*/ None,
            &[],
        ));
        let step_context = step_context.with_tool_router_for_test(router);
        let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));

        let worker = session.services.code_mode_service.start_turn_worker(
            &session,
            Arc::clone(&step_context),
            ToolCallRuntime::new(Arc::clone(&session), step_context, tracker),
        );

        assert!(worker.is_some());
    }

    #[test]
    fn build_nested_tool_payload_uses_function_kind() {
        let payload = build_nested_tool_payload(
            CodeModeToolKind::Function,
            &ToolName::plain("example"),
            Some(json!({ "value": 1 })),
        )
        .expect("function payload should serialize");

        match payload {
            ToolPayload::Function { arguments } => {
                assert_eq!(arguments, r#"{"value":1}"#.to_string());
            }
            other => panic!("expected function payload, got {other:?}"),
        }
    }

    #[test]
    fn build_nested_tool_payload_uses_freeform_kind() {
        let payload = build_nested_tool_payload(
            CodeModeToolKind::Freeform,
            &ToolName::plain("example"),
            Some(json!("hello")),
        )
        .expect("freeform payload should preserve string input");

        match payload {
            ToolPayload::Custom { input } => {
                assert_eq!(input, "hello".to_string());
            }
            other => panic!("expected freeform payload, got {other:?}"),
        }
    }

    #[test]
    fn truncated_sections_share_the_budget_and_recover_only_truncated_text() {
        use super::output_sections::OutputRetention;
        use super::output_sections::truncate_sections;
        use codex_code_mode::FunctionCallOutputContentItem as Item;

        let runtime = tempfile::tempdir().expect("test runtime");
        let retention =
            OutputRetention::in_test_runtime(runtime.path().to_path_buf()).expect("retention");
        let first = format!("first-head{}first-tail", "A".repeat(100));
        let second = format!("βhead{}βtail", "β".repeat(100));
        let middle = format!("middle-head{}middle-tail", "M".repeat(100));
        let last = format!("last-head{}last-tail", "L".repeat(100));
        let media = vec![
            Item::InputImage {
                image_url: "data:image/png;base64,image".to_string(),
                detail: None,
            },
            Item::InputAudio {
                audio_url: "data:audio/wav;base64,audio".to_string(),
            },
        ];
        let output = truncate_sections(
            vec![
                Item::InputText {
                    text: first.clone(),
                    max_tokens: Some(4),
                },
                media[0].clone(),
                Item::InputTextBatch {
                    sections: vec![second.clone(), middle.clone(), last.clone()],
                    max_tokens: Some(12),
                },
                media[1].clone(),
                Item::InputText {
                    text: "tiny".to_string(),
                    max_tokens: None,
                },
            ],
            Some(16),
            codex_protocol::protocol::TruncationPolicy::Tokens(10_000),
            &retention,
        );
        let Item::InputText { text: notice, .. } = &output[0] else {
            panic!("missing notice")
        };
        assert!(notice.starts_with("Warning: truncated 4 text sections"));
        assert!(notice.contains("Section 1: 120 bytes → 15 bytes"));
        let path = std::path::PathBuf::from(
            notice
                .lines()
                .find_map(|line| line.strip_prefix("Truncated emitted text: "))
                .expect("recovery path")
                .split_once(" (Codex host filesystem")
                .expect("host location")
                .0,
        );
        let retained = std::fs::read_to_string(&path).expect("read recovery");
        assert!(retained.contains(&first));
        assert!(retained.contains(&second));
        assert!(retained.contains(&middle));
        assert!(retained.contains(&last));
        assert!(!retained.contains("tiny"));
        assert!(!retained.contains("data:image"));
        assert_eq!(output[2], media[0]);
        assert_eq!(output[6], media[1]);
        let snippets: Vec<_> = output
            .iter()
            .skip(1)
            .filter_map(|item| match item {
                Item::InputText { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert!(snippets.iter().map(|text| text.len()).sum::<usize>() <= 64);
        assert!(snippets[0].starts_with("first"));
        assert!(snippets[0].ends_with("tail"));
        assert!(snippets[1].starts_with("βhead"));
        assert!(snippets[1].ends_with("βtail"));
        assert!(snippets[2].starts_with("middle"));
        assert!(snippets[2].ends_with("tail"));
        assert_eq!(snippets[4], "tiny");
        let group_only = truncate_sections(
            vec![Item::InputTextBatch {
                sections: vec![second, middle, last],
                max_tokens: Some(12),
            }],
            Some(1000),
            codex_protocol::protocol::TruncationPolicy::Tokens(10_000),
            &retention,
        );
        let group_bytes = group_only
            .iter()
            .skip(1)
            .map(|item| match item {
                Item::InputText { text, .. } => text.len(),
                _ => panic!("unflattened batch"),
            })
            .sum::<usize>();
        assert!(
            group_bytes <= 48,
            "the emission cap must not multiply by the batch size"
        );
        // A zero allocation still identifies a nonempty omitted section. History
        // drops empty text items, which used to silently renumber later sections.
        let clipped = truncate_sections(
            vec![
                Item::InputText {
                    text: String::new(),
                    max_tokens: None,
                },
                Item::InputText {
                    text: "αβ".to_string(),
                    max_tokens: None,
                },
                Item::InputText {
                    text: "second".to_string(),
                    max_tokens: None,
                },
            ],
            Some(1000),
            codex_protocol::protocol::TruncationPolicy::Bytes(1),
            &retention,
        );
        let Item::InputText { text: notice, .. } = &clipped[0] else {
            panic!("missing reduction notice")
        };
        assert!(notice.contains(
            "requested output cap 1000 tokens reduced by model policy to 1 approximate text bytes"
        ));
        assert!(notice.contains("truncated 2 text sections"));
        assert_eq!(clipped.len(), 3);
        for item in &clipped[1..] {
            assert!(matches!(item, Item::InputText { text, .. } if text == "…"));
        }
        let capped = truncate_sections(
            vec![Item::InputText {
                text: "tiny".to_string(),
                max_tokens: None,
            }],
            Some(1000),
            codex_protocol::protocol::TruncationPolicy::Tokens(1),
            &retention,
        );
        assert!(
            matches!(&capped[0], Item::InputText { text, .. } if text.starts_with("Warning: requested output cap"))
        );
        assert!(matches!(&capped[1], Item::InputText { text, .. } if text == "tiny"));
        let implicit = truncate_sections(
            vec![Item::InputText {
                text: "tiny".to_string(),
                max_tokens: None,
            }],
            None,
            codex_protocol::protocol::TruncationPolicy::Tokens(1),
            &retention,
        );
        assert_eq!(
            implicit.len(),
            1,
            "an implicit default is not a reduced request"
        );
        assert!(matches!(&implicit[0], Item::InputText { text, .. } if text == "tiny"));
        for _ in 0..32 {
            truncate_sections(
                vec![Item::InputText {
                    text: "x".repeat(100),
                    max_tokens: None,
                }],
                Some(1),
                codex_protocol::protocol::TruncationPolicy::Tokens(10_000),
                &retention,
            );
        }
        assert!(!path.exists(), "oldest artifact must be evicted");
        let directory = path.parent().expect("retention directory").to_path_buf();
        assert!(directory.exists());
        retention.clear();
        assert!(
            !directory.exists(),
            "thread shutdown must remove retained output"
        );
    }
}
