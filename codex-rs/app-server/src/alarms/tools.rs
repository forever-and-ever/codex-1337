use codex_extension_api::ThreadStopInput;

use std::fmt::Write as _;

use std::sync::Arc;

use chrono::DateTime;
use codex_app_server_protocol::DynamicToolFunctionSpec;
use codex_core::config::Config;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::FunctionCallError;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadResumeInput;
use codex_extension_api::ThreadStartInput;
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
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::SessionSource;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolExposure;
use codex_tools::ToolPayload;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::json;

use super::Alarms;
use super::now;
use super::wire;

const DESCRIPTION: &str = "Wake this conversation later. Arm an alarm, then end your turn; when it sounds, a new turn starts with its result as a later output of alarm.set. Prefer this to sleeping or polling inside a turn when a wait may exceed a minute or depends on a background command.";
const SET_DESCRIPTION: &str = "Arm a one-shot alarm, then end your turn normally. It sounds at the first of its conditions: the time (after_seconds or at), or, with session_id, when that background exec_command session exits or prints a new line matching pattern. A watch with a time also sounds at that time if nothing happened first. Clock alarms survive the frontend closing and server restarts; a watched session cannot survive a server restart, and you are told if it is lost.";

pub(crate) fn install(builder: &mut ExtensionRegistryBuilder<Config>, alarms: Alarms) {
    let extension = Arc::new(AlarmTools(alarms));
    builder.tool_contributor(extension.clone());
    builder.thread_lifecycle_contributor(extension);
}

struct AlarmTools(Alarms);

#[derive(Clone)]
enum Access {
    Available,
    Unavailable,
}

impl ThreadLifecycleContributor<Config> for AlarmTools {
    fn on_thread_start<'a>(
        &'a self,
        input: ThreadStartInput<'a, Config>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let available = !input.config.ephemeral
                && input.persistent_thread_state_available
                && !input.session_source.is_non_root_agent()
                && !matches!(input.session_source, SessionSource::Exec)
                && self.0.available();
            input.thread_store.insert(if available {
                Access::Available
            } else {
                Access::Unavailable
            });
            if available {
                if let Ok(id) = ThreadId::from_string(input.thread_store.level_id())
                    && let Err(error) = self.0.attach_hold(id, input.thread_store).await
                {
                    tracing::error!(%error, "cannot bind alarm runtime");
                }
                if let Err(error) = self.0.initialize().await {
                    tracing::error!(%error, "alarm registry unavailable during thread start");
                }
            }
        })
    }
    fn on_thread_resume<'a>(&'a self, input: ThreadResumeInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if matches!(
                input.thread_store.get::<Access>().as_deref(),
                Some(Access::Available)
            ) && let Ok(id) = ThreadId::from_string(input.thread_store.level_id())
                && let Err(error) = self.0.attach_hold(id, input.thread_store).await
            {
                tracing::error!(%error, "cannot bind resumed alarm runtime");
            }
        })
    }
    fn on_thread_stop<'a>(&'a self, input: ThreadStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if let Ok(id) = ThreadId::from_string(input.thread_store.level_id()) {
                self.0.detach_hold(id, input.thread_store).await;
            }
        })
    }
}

impl ToolContributor for AlarmTools {
    fn tools(
        &self,
        _session: &ExtensionData,
        thread: &ExtensionData,
    ) -> Vec<Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>> {
        if !matches!(thread.get::<Access>().as_deref(), Some(Access::Available)) {
            return Vec::new();
        }
        let Ok(caller) = ThreadId::from_string(thread.level_id()) else {
            return Vec::new();
        };
        definitions()
            .into_iter()
            .map(|function| {
                Arc::new(AlarmTool {
                    alarms: self.0.clone(),
                    caller,
                    function,
                }) as Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>
            })
            .collect()
    }
}

struct AlarmTool {
    alarms: Alarms,
    caller: ThreadId,
    function: DynamicToolFunctionSpec,
}

impl<'call> ToolExecutor<ToolCall<'call>> for AlarmTool {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced("alarm", &self.function.name)
    }
    #[expect(
        clippy::expect_used,
        reason = "Alarm schemas are static owned definitions"
    )]
    fn spec(&self) -> ToolSpec {
        ToolSpec::Namespace(ResponsesApiNamespace {
            name: "alarm".into(),
            description: DESCRIPTION.into(),
            tools: vec![ResponsesApiNamespaceTool::Function(
                codex_tools::dynamic_tool_to_responses_api_tool(&self.function)
                    .expect("owned alarm schema is valid"),
            )],
        })
    }
    fn exposure(&self) -> ToolExposure {
        ToolExposure::Deferred
    }
    fn approval(&self) -> Option<ToolApproval> {
        Some(ToolApproval {
            namespace: "alarm".into(),
            description: self.function.description.clone(),
            default_mode: ToolApprovalMode::Approve,
            read_only: self.supports_parallel_tool_calls(),
        })
    }
    fn supports_parallel_tool_calls(&self) -> bool {
        self.function.name == "list"
    }
    fn handle<'a>(&'a self, call: ToolCall<'call>) -> ToolExecutorFuture<'a>
    where
        'call: 'a,
    {
        Box::pin(async move {
            let args = call.function_arguments()?;
            let result = Box::pin(self.execute(args))
                .await
                .map_err(FunctionCallError::RespondToModel)?;
            Ok(Box::new(Porcelain(result)) as Box<dyn ToolOutput>)
        })
    }
}

impl AlarmTool {
    async fn execute(&self, args: &str) -> Result<String, String> {
        self.alarms.initialize().await?;
        let service = self
            .alarms
            .0
            .coordination
            .get()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| "alarm host is unavailable".to_string())?;
        let (thread, _lease) = Box::pin(service.load(&self.caller.to_string()))
            .await
            .map_err(|err| err.message)?;
        let config = thread.config_snapshot().await;
        if config.ephemeral
            || config.session_source.is_non_root_agent()
            || matches!(config.session_source, SessionSource::Exec)
        {
            return Err(
                "Alarms require a root conversation hosted by a durable app server.".into(),
            );
        }
        self.alarms
            .attach_hold(self.caller, thread.thread_extension_data())
            .await?;
        match self.function.name.as_str() {
            "list" => {
                let _: Empty = serde_json::from_str(args).map_err(|err| err.to_string())?;
                Ok(wire::armed_list(&self.alarms.summaries(self.caller)))
            }
            "disarm" => {
                let args: Disarm = serde_json::from_str(args).map_err(|err| err.to_string())?;
                let armed = self.alarms.disarm(self.caller, &args.id).await?;
                Ok(format!(
                    "Disarmed {}.\n{}",
                    args.id,
                    wire::armed_list(&armed)
                ))
            }
            "set" => {
                let args: Set = serde_json::from_str(args).map_err(|err| err.to_string())?;
                let SetRequest { note, condition } = args.validate()?;
                let (arm, stale) = match condition {
                    Condition::Watch {
                        session_id,
                        pattern,
                        deadline,
                    } => {
                        let watch = thread
                            .watch_background_terminal(session_id, pattern.clone())
                            .await
                            .map_err(|err| err.to_string())?;
                        let stale = pattern
                            .as_ref()
                            .and_then(|pattern| {
                                watch
                                    .retained_output()
                                    .lines()
                                    .find(|line| pattern.is_match(line))
                            })
                            .map(|line| line.chars().take(500).collect::<String>());
                        (
                            super::Arm::Watch {
                                session_id,
                                watch,
                                deadline,
                            },
                            stale,
                        )
                    }
                    Condition::Clock { at } => (super::Arm::Clock { at }, None),
                };
                // A fresh thread's database projection may not exist before its first turn flush.
                thread.ensure_rollout_materialized().await;
                let alarm = self.alarms.arm(self.caller, &note, arm).await?;
                let summary = wire::summary(&alarm)
                    .ok_or_else(|| "newly armed alarm has no condition".to_string())?;
                let mut text = format!(
                    "Armed {}: {}.\n{}",
                    summary.id,
                    wire::condition(&summary.condition),
                    wire::armed_list(&self.alarms.summaries(self.caller))
                );
                if let Some(line) = stale {
                    let _ = write!(
                        text,
                        "\nAlready in its output: `{line}`. Only new output sounds this alarm."
                    );
                }
                Ok(text)
            }
            name => Err(format!("Unsupported alarm tool: {name}")),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    note: String,
    after_seconds: Option<i64>,
    at: Option<String>,
    session_id: Option<i32>,
    pattern: Option<String>,
}

struct SetRequest {
    note: String,
    condition: Condition,
}

enum Condition {
    Clock {
        at: i64,
    },
    Watch {
        session_id: i32,
        pattern: Option<regex::Regex>,
        deadline: Option<i64>,
    },
}

impl Set {
    fn validate(self) -> Result<SetRequest, String> {
        if !(1..=2000).contains(&self.note.chars().count()) {
            return Err("note must contain 1 to 2000 characters".into());
        }
        let at = match (self.after_seconds, self.at) {
            (Some(seconds), None) if (1..=604_800).contains(&seconds) => {
                Some(now() + seconds * 1000)
            }
            (Some(_), None) => return Err("after_seconds must be between 1 and 604800".into()),
            (None, Some(at)) => {
                let at = DateTime::parse_from_rfc3339(&at)
                    .map_err(|err| format!("at must be an RFC 3339 timestamp with offset: {err}"))?
                    .timestamp_millis();
                let delay = at - now();
                if !(1..=604_800_000).contains(&delay) {
                    return Err("at must be in the future and within 7 days".into());
                }
                Some(at)
            }
            (Some(_), Some(_)) => return Err("after_seconds and at are mutually exclusive".into()),
            (None, None) => None,
        };
        if at.is_none() && self.session_id.is_none() {
            return Err("Provide after_seconds, at, or session_id.".into());
        }
        if self.pattern.is_some() && self.session_id.is_none() {
            return Err("pattern requires session_id".into());
        }
        let pattern = self
            .pattern
            .map(|pattern| {
                if pattern.chars().count() > 512 {
                    return Err("pattern must contain at most 512 characters".into());
                }
                RegexBuilder::new(&pattern)
                    .size_limit(1_048_576)
                    .dfa_size_limit(1_048_576)
                    .build()
                    .map_err(|err| format!("invalid pattern: {err}"))
            })
            .transpose()?;
        let condition = match (self.session_id, at) {
            (Some(session_id), deadline) => Condition::Watch {
                session_id,
                pattern,
                deadline,
            },
            (None, Some(at)) => Condition::Clock { at },
            (None, None) => unreachable!("validated condition"),
        };
        Ok(SetRequest {
            note: self.note,
            condition,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Disarm {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

struct Porcelain(String);
impl codex_tools::HookToolOutput for Porcelain {}

impl ToolOutput for Porcelain {
    fn log_output(&self) -> String {
        self.0.clone()
    }
    fn success_for_logging(&self) -> bool {
        true
    }
    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(self.0.clone()),
                success: Some(true),
            },
        }
    }
}

fn definitions() -> Vec<DynamicToolFunctionSpec> {
    [
        ("set", SET_DESCRIPTION, json!({"note":{"type":"string","minLength":1,"maxLength":2000,"description":"What to do when woken, written for yourself after a long gap."},"after_seconds":{"type":"integer","minimum":1,"maximum":604_800},"at":{"type":"string","description":"RFC 3339 timestamp with offset, in the future and within 7 days."},"session_id":{"type":"integer","description":"A running background exec_command session of this conversation."},"pattern":{"type":"string","maxLength":512,"description":"Rust regex syntax, matched per new output line. Requires session_id."}}), vec!["note"]),
        ("disarm", "Disarm an unsounded alarm by id.", json!({"id":{"type":"string"}}), vec!["id"]),
        ("list", "List this conversation's armed alarms.", json!({}), Vec::new()),
    ].into_iter().map(|(name, description, properties, required)| DynamicToolFunctionSpec { name: name.into(), description: description.into(), input_schema: json!({"type":"object","additionalProperties":false,"properties":properties,"required":required}), defer_loading: true }).collect()
}
