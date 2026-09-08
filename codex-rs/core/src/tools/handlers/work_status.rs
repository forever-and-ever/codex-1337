use super::parse_arguments;
use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WorkStatus;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use std::collections::BTreeMap;

pub struct WorkStatusHandler;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    activity: String,
}

impl ToolExecutor<ToolInvocation> for WorkStatusHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("update_work_status")
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: "update_work_status".into(),
            description: "Show what you are currently working on without posting a chat message. Use at the start of substantive work and when your activity changes, instead of routine progress-only chat updates. Keep it concrete, not private reasoning: a short, nonempty summary of at most 160 characters. Replaces the previous activity and clears when the turn ends. Never changes the goal.".into(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([
                    ("activity".into(), JsonSchema::string(Some("What you are currently working on.".into()))),
                ]),
                Some(vec!["activity".into()]),
                /*additional_properties*/ Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let ToolPayload::Function { arguments } = &invocation.payload else {
                return Err(FunctionCallError::RespondToModel(
                    "Expected function arguments.".into(),
                ));
            };
            let update: Update = parse_arguments(arguments)?;
            let status = WorkStatus {
                turn_id: invocation.turn.sub_id.clone(),
                activity: summary(update.activity)?,
            };
            invocation
                .session
                .send_event(&invocation.turn, EventMsg::WorkStatusUpdated(status))
                .await;
            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                "Work status updated.".into(),
                /*success*/ Some(true),
            )))
        })
    }
}

impl CoreToolRuntime for WorkStatusHandler {
    fn is_builtin_control_tool(&self) -> bool {
        true
    }
}

fn summary(text: String) -> Result<String, FunctionCallError> {
    if text.chars().count() > 160 {
        return Err(FunctionCallError::RespondToModel(
            "Keep each summary within 160 characters.".into(),
        ));
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(FunctionCallError::RespondToModel(
            "Summaries must contain visible text without control characters.".into(),
        ));
    }
    Ok(text)
}
