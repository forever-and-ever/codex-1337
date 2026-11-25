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
            description: format!(
                "Set `activity` to show what you are working on, without posting a chat message. Call it at the start of substantive work and whenever your activity changes, in place of progress-only chat updates. The activity is a concrete summary of at most {} characters, not private reasoning; it replaces the previous one and clears when the turn ends. A longer summary is truncated and a malformed update ignored, each with a warning and never a failed call. It never changes the goal.",
                WorkStatus::MAX_ACTIVITY_CHARS
            ),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([(
                    "activity".into(),
                    JsonSchema::string(Some("What you are currently working on.".into())),
                )]),
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
            let reply = |text: String| {
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    text,
                    /*success*/ Some(true),
                )))
            };
            if invocation.turn.session_source.is_internal()
                || crate::guardian::is_basic_session_source(&invocation.turn.session_source)
            {
                return reply("Work status suppressed for this session.".into());
            }
            let ToolPayload::Function { arguments } = &invocation.payload else {
                return reply("Warning: work status ignored; expected function arguments.".into());
            };
            let update: Update = match parse_arguments(arguments) {
                Ok(update) => update,
                Err(error) => return reply(format!("Warning: work status ignored; {error}")),
            };
            let activity = match summary(&update.activity) {
                Ok(activity) => activity,
                Err(error) => return reply(format!("Warning: work status ignored; {error}")),
            };
            let truncated = activity.chars().count() > WorkStatus::MAX_ACTIVITY_CHARS;
            let status = WorkStatus {
                turn_id: invocation.turn.sub_id.clone(),
                activity: activity
                    .chars()
                    .take(WorkStatus::MAX_ACTIVITY_CHARS)
                    .collect(),
                emitted_at: Some(chrono::Utc::now().timestamp()),
            };
            invocation
                .session
                .send_event(&invocation.turn, EventMsg::WorkStatusUpdated(status))
                .await;
            reply(if truncated {
                format!(
                    "Warning: work status truncated to {} characters.",
                    WorkStatus::MAX_ACTIVITY_CHARS
                )
            } else {
                "Work status updated.".into()
            })
        })
    }
}

impl CoreToolRuntime for WorkStatusHandler {
    fn is_builtin_control_tool(&self) -> bool {
        true
    }
}

fn summary(text: &str) -> Result<String, FunctionCallError> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(FunctionCallError::RespondToModel(
            "Summaries must contain visible text without control characters.".into(),
        ));
    }
    Ok(text)
}
