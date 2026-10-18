//! Exact, bounded filesystem transfers for Code Mode, through executor authority.

use crate::sandboxing::SandboxPermissions;
use codex_file_system::EnvironmentAccess;
use codex_file_system::WriteFileOptions;
use codex_tools::FunctionCallError;
use codex_tools::JsonSchema;
use codex_tools::JsonToolOutput;
use codex_tools::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;

use super::apply_granted_turn_permissions;
use super::resolve_tool_environment;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::CoreToolRuntime;

mod changes;

const MAX_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Read {
        path: String,
        environment_id: Option<String>,
        encoding: Option<Encoding>,
    },
    Write {
        path: String,
        environment_id: Option<String>,
        data: Data,
    },
}

#[derive(Deserialize)]
enum Encoding {
    #[serde(rename = "utf8")]
    Utf8,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Data {
    Text(String),
    Bytes(Vec<u8>),
}

pub(crate) struct FileSystemHandler;

impl ToolExecutor<ToolInvocation> for FileSystemHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("fs")
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(codex_tools::ResponsesApiTool {
            name: "fs".into(),
            description: "Filesystem transport for the fs.readFile/fs.writeFile JS helpers. Uses the selected executor's cwd and sandbox, including granted turn permissions. Exact transfers up to 1 MiB; larger files fail, never truncate. No escalation. Not printed unless explicitly passed to text().".into(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                [
                    ("operation", JsonSchema::string_enum(vec!["read".into(), "write".into()], /*description*/ None)),
                    ("path", JsonSchema::string(None)),
                    ("environment_id", JsonSchema::string(None)),
                    ("encoding", JsonSchema::string_enum(vec!["utf8".into()], /*description*/ None)),
                    ("data", JsonSchema::any_of(vec![
                        JsonSchema::string(None),
                        JsonSchema::array(JsonSchema::integer(Some("Byte, 0 through 255.".into())), /*description*/ None),
                    ], /*description*/ None)),
                ].into_iter().map(|(name, schema)| (name.into(), schema)).collect(),
                Some(vec!["operation".into(), "path".into()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            if !matches!(
                invocation.source,
                crate::tools::router::ToolCallSource::CodeMode { .. }
            ) {
                return Err(fault("fs is available only inside exec"));
            }
            let ToolPayload::Function { arguments } = &invocation.payload else {
                return Err(fault("fs expects JSON arguments"));
            };
            let request: Request = serde_json::from_str(arguments).map_err(fault)?;
            let (path, environment_id) = match &request {
                Request::Read {
                    path,
                    environment_id,
                    ..
                }
                | Request::Write {
                    path,
                    environment_id,
                    ..
                } => (path, environment_id),
            };
            let environment = resolve_tool_environment(
                &invocation.step_context,
                environment_id.as_deref(),
                "no filesystem execution environment",
            )?;
            let path = environment.cwd().join(path).map_err(fault)?;
            let grants = apply_granted_turn_permissions(
                &invocation.step_context,
                environment,
                environment.cwd(),
                SandboxPermissions::UseDefault,
                /*additional_permissions*/ None,
            );
            let access = environment.fs_accessor(grants.additional_permissions);
            let value = match request {
                Request::Read { encoding, .. } => {
                    let mut stream = access.read_file_stream(&path).await.map_err(fault)?;
                    let mut bytes = Vec::new();
                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk.map_err(fault)?;
                        if chunk.len() > MAX_BYTES - bytes.len() {
                            return Err(fault(
                                "fs.readFile exceeds 1 MiB; use a script for larger files",
                            ));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    match encoding {
                        Some(Encoding::Utf8) => {
                            Value::String(String::from_utf8(bytes).map_err(fault)?)
                        }
                        None => serde_json::to_value(bytes).map_err(fault)?,
                    }
                }
                Request::Write { data, .. } => {
                    let bytes = match data {
                        Data::Text(text) => text.into_bytes(),
                        Data::Bytes(bytes) => bytes,
                    };
                    if bytes.len() > MAX_BYTES {
                        return Err(fault(
                            "fs.writeFile exceeds 1 MiB; use a script for larger files",
                        ));
                    }
                    let change = changes::WriteChange::capture(&access, &path, &bytes).await;
                    if let Err(error) = access
                        .write_file(&path, bytes, WriteFileOptions::default())
                        .await
                    {
                        // A failing write may have truncated or partially replaced the file.
                        changes::WriteChange::Unavailable
                            .publish(&invocation, &environment.selection.environment_id)
                            .await;
                        return Err(fault(error));
                    }
                    change
                        .publish(&invocation, &environment.selection.environment_id)
                        .await;
                    Value::Null
                }
            };
            Ok(Box::new(JsonToolOutput::new(value)) as Box<dyn codex_tools::ToolOutput>)
        })
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }
}

impl CoreToolRuntime for FileSystemHandler {}

fn fault(error: impl std::fmt::Display) -> FunctionCallError {
    FunctionCallError::RespondToModel(format!("filesystem: {error}"))
}
