use super::CoreToolRuntime;
use super::Deserialize;
use super::FunctionCallError;
use super::ToolExecutor;
use super::ToolInvocation;
use super::ToolName;
use super::ToolPayload;
use super::boxed_tool_output;
use super::function_arguments;
use super::parse_arguments;
use super::resolve_agent_target;
use crate::tools::context::FunctionToolOutput;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub(crate) struct Handler;

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("retire_agent")
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: "retire_agent".into(),
            description: "Retire a stopped descendant and its stopped descendants from the persistent family roster. Refuses working agents; interrupt them first. Saved conversations are preserved. Retirement persists across server restarts.".into(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(BTreeMap::from([
                ("target".into(), JsonSchema::string(Some("Descendant agent ID or relative/canonical task path.".into()))),
            ]), Some(vec!["target".into()]), Some(false.into())),
            output_schema: None,
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                target: String,
            }
            let args: Args = parse_arguments(&function_arguments(invocation.payload)?)?;
            let target =
                resolve_agent_target(&invocation.session, &invocation.turn, &args.target).await?;
            let retired = invocation
                .session
                .services
                .agent_control
                .retire_agent(invocation.session.thread_id, target)
                .await
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                format!(
                    "Retired {} agent(s). Saved conversations were preserved.",
                    retired.len()
                ),
                Some(true),
            )))
        })
    }
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}
