//! Server-owned coordination tools; frontend lifetimes are not capabilities.

use crate::request_processors::ThreadCoordinationService;
use codex_app_server_protocol::DynamicToolFunctionSpec;
use codex_app_server_protocol::DynamicToolNamespaceTool;
use codex_app_server_protocol::DynamicToolSpec;
use codex_app_server_protocol::SortDirection;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadQueueAddParams;
use codex_app_server_protocol::ThreadSetNameParams;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::ThreadTurnsListParams;
use codex_app_server_protocol::ThreadUnarchiveParams;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnToolOutput;
use codex_app_server_protocol::delegation::DelegatedMessage;
use codex_app_server_protocol::delegation::MAX_SENDER_NAME_CHARS;
use codex_core::config::Config;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::FunctionCallError;
use codex_extension_api::JsonToolOutput;
use codex_extension_api::ToolApproval;
use codex_extension_api::ToolApprovalMode;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolContributor;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolExecutorFuture;
use codex_extension_api::ToolName;
use codex_extension_api::ToolOutput;
use codex_extension_api::ToolSpec;
use codex_protocol::ThreadId;
use codex_protocol::models::FunctionCallOutputBody;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolExposure;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::Weak;
use uuid::Uuid;

mod mcp_isolation;
mod queries;
mod render;
mod response;
mod spec;

// Preserve historical callable names for cached model/Code Mode contexts.
const NAMESPACE: &str = "mcp__codex_tui";
// Configuration, approval, and persisted delegation identity remain transport-free.
const POLICY_NAMESPACE: &str = "codex_tui";
const DELEGATION_TOOLS: [&str; 3] = ["create_thread", "send_message_to_thread", "fork_thread"];
const DEFAULT_LIST_LIMIT: u32 = 10;
const MAX_LIST_LIMIT: u32 = 50;
const DEFAULT_READ_TURN_LIMIT: u32 = 1;
const MAX_READ_TURN_LIMIT: u32 = 10;
const DEFAULT_OUTPUT_CHARS: usize = 2_000;
const MAX_OUTPUT_CHARS: usize = 20_000;
const MAX_ERROR_CHARS: usize = 248;
const MAX_INPUT_BYTES: usize = 1_000;
const MAX_DELEGATED_INPUT_BYTES: usize = (MAX_INPUT_BYTES + MAX_SENDER_NAME_CHARS) * 5 + 256;
const MAX_WAIT_TARGETS: usize = 8;
const MAX_WAIT_TIMEOUT_MS: u64 = 120_000;

pub(crate) type CoordinationSlot = Arc<OnceLock<Weak<ThreadCoordinationService>>>;

pub(crate) fn install(builder: &mut ExtensionRegistryBuilder<Config>, service: CoordinationSlot) {
    builder.tool_contributor(Arc::new(CoordinationTools { service }));
    builder.mcp_server_contributor(Arc::new(mcp_isolation::CoordinationMcpIsolation));
}

struct CoordinationTools {
    service: CoordinationSlot,
}

impl ToolContributor for CoordinationTools {
    fn tools(
        &self,
        _session: &ExtensionData,
        thread: &ExtensionData,
    ) -> Vec<Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>> {
        let Ok(caller) = ThreadId::from_string(thread.level_id()) else {
            return Vec::new();
        };
        spec::tool_specs()
            .into_iter()
            .flat_map(|spec| match spec {
                DynamicToolSpec::Namespace(namespace) => namespace.tools,
                DynamicToolSpec::Function(function) => {
                    vec![DynamicToolNamespaceTool::Function(function)]
                }
            })
            .map(|DynamicToolNamespaceTool::Function(function)| {
                Arc::new(CoordinationTool {
                    service: Arc::clone(&self.service),
                    caller,
                    function,
                }) as Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>
            })
            .collect()
    }
}

struct CoordinationTool {
    service: CoordinationSlot,
    caller: ThreadId,
    function: DynamicToolFunctionSpec,
}

impl<'call> ToolExecutor<ToolCall<'call>> for CoordinationTool {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(NAMESPACE, &self.function.name)
    }
    fn spec(&self) -> ToolSpec {
        let function: codex_protocol::dynamic_tools::DynamicToolFunctionSpec =
            self.function.clone().into();
        ToolSpec::Namespace(ResponsesApiNamespace {
            name: NAMESPACE.to_owned(),
            description: "Manage Codex tasks available through the connected app server.".into(),
            tools: vec![ResponsesApiNamespaceTool::Function(
                codex_tools::dynamic_tool_to_responses_api_tool(&function)
                    .expect("coordination tool schema is valid"),
            )],
        })
    }
    fn exposure(&self) -> ToolExposure {
        ToolExposure::Deferred
    }
    fn approval(&self) -> Option<ToolApproval> {
        Some(ToolApproval {
            namespace: POLICY_NAMESPACE.into(),
            description: self.function.description.clone(),
            default_mode: if DELEGATION_TOOLS.contains(&self.function.name.as_str()) {
                ToolApprovalMode::Prompt
            } else {
                ToolApprovalMode::Approve
            },
            read_only: self.supports_parallel_tool_calls(),
        })
    }
    fn supports_parallel_tool_calls(&self) -> bool {
        matches!(
            self.function.name.as_str(),
            "list_threads" | "list_archived_threads" | "read_thread" | "wait_threads"
        )
    }
    fn handle<'a>(&'a self, call: ToolCall<'call>) -> ToolExecutorFuture<'a>
    where
        'call: 'a,
    {
        Box::pin(async move {
            let service = self.service.get().and_then(Weak::upgrade).ok_or_else(|| {
                FunctionCallError::RespondToModel("coordination service is unavailable".into())
            })?;
            let arguments = serde_json::from_str(call.function_arguments()?).map_err(|err| {
                FunctionCallError::RespondToModel(format!("invalid arguments: {err}"))
            })?;
            let result = self
                .execute(&service, &call.turn_id, arguments)
                .await
                .map_err(|err| {
                    FunctionCallError::RespondToModel(render::truncate(&err, MAX_ERROR_CHARS))
                })?;
            let text = response::serialize(result).map_err(FunctionCallError::RespondToModel)?;
            let result = serde_json::from_str(&text)
                .map_err(|err| FunctionCallError::Fatal(err.to_string()))?;
            Ok(Box::new(JsonToolOutput::new(result)) as Box<dyn ToolOutput>)
        })
    }
}

impl CoordinationTool {
    async fn execute(
        &self,
        service: &ThreadCoordinationService,
        turn_id: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        let caller = self.caller.to_string();
        match self.function.name.as_str() {
            "list_threads" | "list_archived_threads" => {
                queries::list(service, &self.function.name, arguments).await
            }
            "read_thread" => queries::read(service, arguments).await,
            "wait_threads" => queries::wait(service, &caller, arguments).await,
            "send_message_to_thread" => {
                let args: SendArguments = parse(arguments)?;
                if args.delivery == MessageDelivery::Queued && args.model.is_some() {
                    return Err("Queued messages use the destination's model; omit model.".into());
                }
                if args.model.as_deref().is_some_and(str::is_empty) {
                    return Err("model must not be empty".into());
                }
                // Execution is already approved here. Check before either
                // delivery path can restore the destination or mutate its queue.
                service
                    .ensure_message_recipient(&args.thread_id)
                    .await
                    .map_err(error)?;
                let output =
                    delegated_output(service, &caller, &self.function.name, args.prompt).await?;
                if args.delivery == MessageDelivery::Queued {
                    let queued = service
                        .enqueue(ThreadQueueAddParams {
                            thread_id: args.thread_id.clone(),
                            input: None,
                            client_user_message_id: Uuid::now_v7().to_string(),
                            tool_output: Some(Box::new(output)),
                        })
                        .await
                        .map_err(error)?;
                    Ok(
                        json!({"threadId": args.thread_id, "delivery": "queued", "queuedSubmissionId": queued.queued_submission.id}),
                    )
                } else {
                    service
                        .send(&args.thread_id, output, args.model)
                        .await
                        .map_err(error)?;
                    Ok(json!({"threadId": args.thread_id}))
                }
            }
            "create_thread" => {
                let args: CreateArguments = parse(arguments)?;
                if args
                    .title
                    .as_deref()
                    .is_some_and(|title| title.trim().is_empty())
                {
                    return Err("title must not be empty".into());
                }
                let output =
                    delegated_output(service, &caller, &self.function.name, args.prompt).await?;
                let (started, _runtime_lease) = service
                    .create(self.caller, args.model)
                    .await
                    .map_err(error)?;
                let thread_id = started.thread.id;
                if let Some(title) = args.title {
                    if let Err(error) = service
                        .rename(ThreadSetNameParams {
                            thread_id: thread_id.clone(),
                            name: title.trim().to_owned(),
                        })
                        .await
                    {
                        tracing::warn!(
                            thread_id,
                            error = error.message,
                            "failed to name independent task"
                        );
                    }
                }
                service
                    .send(&thread_id, output, /*model*/ None)
                    .await
                    .map_err(error)?;
                Ok(json!({"threadId": thread_id}))
            }
            "fork_thread" => {
                let args: ForkArguments = parse(arguments)?;
                let thread_id = args.thread_id.unwrap_or(caller);
                let thread = service.read(&thread_id).await.map_err(error)?;
                let before_turn_id = if same_thread(&thread_id, &self.caller.to_string()) {
                    Some(turn_id.to_owned())
                } else if matches!(thread.status, ThreadStatus::Active { .. }) {
                    service
                        .turns(ThreadTurnsListParams {
                            thread_id: thread_id.clone(),
                            cursor: None,
                            limit: Some(1),
                            sort_direction: Some(SortDirection::Desc),
                            items_view: Some(TurnItemsView::NotLoaded),
                        })
                        .await
                        .map_err(error)?
                        .data
                        .into_iter()
                        .find(|turn| {
                            turn.status == codex_app_server_protocol::TurnStatus::InProgress
                        })
                        .map(|turn| turn.id)
                } else {
                    None
                };
                let forked = service
                    .fork(ThreadForkParams {
                        thread_id: thread_id.clone(),
                        before_turn_id,
                        ephemeral: thread.ephemeral,
                        exclude_turns: true,
                        ..Default::default()
                    })
                    .await
                    .map_err(error)?;
                Ok(
                    json!({"environment": {"type": "same-directory"}, "sourceThreadId": thread_id, "threadId": forked.thread.id, "continuation": "The fork contains completed history only. If the source thread was running, the active turn and unfinished response are not in the child. Send a follow-up message to threadId only if the task requires work to continue there."}),
                )
            }
            "set_thread_title" => {
                let args: TitleArguments = parse(arguments)?;
                if args.title.trim().is_empty() {
                    return Err("title must not be empty".into());
                }
                let thread_id = args.thread_id.unwrap_or(caller);
                service
                    .rename(ThreadSetNameParams {
                        thread_id: thread_id.clone(),
                        name: args.title.clone(),
                    })
                    .await
                    .map_err(error)?;
                Ok(json!({"threadId": thread_id, "title": args.title}))
            }
            "set_thread_archived" => {
                let args: ArchiveArguments = parse(arguments)?;
                let thread_id = args.thread_id.unwrap_or(caller.clone());
                if args.archived && same_thread(&thread_id, &caller) {
                    return Err("cannot archive the calling task".into());
                }
                if args.archived {
                    service
                        .archive(ThreadArchiveParams {
                            thread_id: thread_id.clone(),
                        })
                        .await
                        .map_err(error)?;
                } else {
                    service
                        .unarchive(ThreadUnarchiveParams {
                            thread_id: thread_id.clone(),
                        })
                        .await
                        .map_err(error)?;
                }
                Ok(json!({"threadId": thread_id, "archived": args.archived}))
            }
            tool => Err(format!("Unsupported coordination tool: {tool}")),
        }
    }
}

async fn delegated_output(
    service: &ThreadCoordinationService,
    caller: &str,
    tool: &str,
    prompt: String,
) -> Result<TurnToolOutput, String> {
    if prompt.trim().is_empty() || prompt.len() > MAX_INPUT_BYTES {
        return Err(format!(
            "prompt must contain between 1 and {MAX_INPUT_BYTES} bytes"
        ));
    }
    let source = service.read(caller).await.map_err(error)?;
    let prompt = DelegatedMessage::new(&source, prompt).encode();
    if prompt.len() > MAX_DELEGATED_INPUT_BYTES {
        return Err("delegated prompt exceeds its envelope limit".into());
    }
    Ok(TurnToolOutput {
        name: tool.to_owned(),
        namespace: Some(POLICY_NAMESPACE.into()),
        output: FunctionCallOutputBody::Text(prompt),
    })
}

fn parse<T: DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|err| err.to_string())
}
fn error(error: codex_app_server_protocol::JSONRPCErrorError) -> String {
    error.message
}
fn same_thread(a: &str, b: &str) -> bool {
    ThreadId::from_string(a)
        .ok()
        .zip(ThreadId::from_string(b).ok())
        .is_some_and(|(a, b)| a == b)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListArguments {
    limit: Option<u32>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadArguments {
    thread_id: String,
    cursor: Option<String>,
    turn_limit: Option<u32>,
    include_outputs: Option<bool>,
    max_output_chars_per_item: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateArguments {
    prompt: String,
    title: Option<String>,
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForkArguments {
    thread_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendArguments {
    thread_id: String,
    prompt: String,
    model: Option<String>,
    #[serde(default)]
    delivery: MessageDelivery,
}

#[derive(Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum MessageDelivery {
    #[default]
    Immediate,
    Queued,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchiveArguments {
    thread_id: Option<String>,
    archived: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TitleArguments {
    thread_id: Option<String>,
    title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitArguments {
    targets: Vec<WaitTarget>,
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitTarget {
    thread_id: String,
    after_cursor: Option<String>,
}
