use super::*;
use crate::agent::control::AgentTierStatus;
use crate::tools::handlers::multi_agents_spec::create_set_agent_service_tier_tool;
use codex_tools::ToolSpec;

pub(crate) struct Handler;

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("set_agent_service_tier")
    }
    fn spec(&self) -> ToolSpec {
        create_set_agent_service_tier_tool()
    }
    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let args: Args = parse_arguments(&function_arguments(invocation.payload)?)?;
            let target =
                resolve_agent_target(&invocation.session, &invocation.turn, &args.target).await?;
            let status = invocation
                .session
                .services
                .agent_control
                .select_agent_service_tier(invocation.session.thread_id, target, &args.service_tier)
                .await
                .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
            Ok(boxed_tool_output(status))
        })
    }
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    target: String,
    service_tier: String,
}

impl ToolOutput for AgentTierStatus {
    fn log_output(&self) -> String {
        tool_output_json_text(self, "set_agent_service_tier")
    }
    fn success_for_logging(&self) -> bool {
        true
    }
    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, Some(true), "set_agent_service_tier")
    }
    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "set_agent_service_tier")
    }
}
