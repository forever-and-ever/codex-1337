use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolEnvironments;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::resolve_tool_environment;
use crate::tools::handlers::unified_exec::ExecCommandHandler;
use crate::tools::handlers::unified_exec::ExecCommandHandlerOptions;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::PostToolUsePayload;
use crate::tools::registry::PreToolUsePayload;
use crate::tools::registry::ToolExecutor;
use crate::tools::registry::ToolRegistry;
use crate::tools::script_exec::ScriptLanguage;
use crate::unified_exec::InitialWait;
use crate::unified_exec::MIN_YIELD_TIME_MS;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

mod parser;

pub(crate) fn register(
    registry: &mut ToolRegistry,
    options: ExecCommandHandlerOptions,
    resumable: bool,
) {
    for language in [ScriptLanguage::Python, ScriptLanguage::Bash] {
        registry.add_with_exposure(
            ScriptExecHandler {
                language,
                options,
                resumable,
            },
            ToolExposure::DirectModelOnly,
        );
    }
}

struct ScriptExecHandler {
    language: ScriptLanguage,
    options: ExecCommandHandlerOptions,
    resumable: bool,
}

impl ToolExecutor<ToolInvocation> for ScriptExecHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(self.language.name())
    }

    fn spec(&self) -> ToolSpec {
        let language = self.language.name();
        let interpreter = match self.language {
            ScriptLanguage::Python => "python3 -u",
            ScriptLanguage::Bash => "bash",
        };
        let environment = if self.options.include_environment_id {
            " `environment_id` selects an executor other than the default."
        } else {
            ""
        };
        let background = if self.resumable {
            "`background: true` returns a session ID for `write_stdin` at once; otherwise the call waits for the process to exit."
        } else {
            "`background: true` is rejected here, and a call is subject to the one-shot execution limit."
        };
        ToolSpec::Freeform(FreeformTool {
            name: language.to_string(),
            description: format!(
                "Run {language} source in a fresh {interpreter} process: no REPL, no state between calls, no packages or venv beyond the environment's own. The input is the source alone, at most 64 KiB, with no JSON wrapper or Markdown fence. An optional first line, after a shebang if there is one, sets options: `# @exec: {{\"workdir\": \"/path\", \"max_output_tokens\": 2000}}`. `workdir` is resolved against the environment's cwd, the default. `max_output_tokens` defaults to 10000, within this model's limit. `sandbox_permissions: \"require_escalated\"` requests unsandboxed execution under the approval policy, and only then may `justification` explain the request.{environment} An unknown, duplicate, null or invalid option rejects the call before anything runs. {background} The process uses the environment's PATH, sandbox, network restrictions and approvals; the source does not arrive on stdin, which is not a TTY. For anything else, including `with_additional_permissions`, use `exec_command`."
            ),
            defer_loading: None,
            format: FreeformToolFormat {
                r#type: "grammar".to_string(),
                syntax: "lark".to_string(),
                definition: "start: SOURCE\nSOURCE: /[\\s\\S]+/".to_string(),
            },
        })
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn handle<'a>(&'a self, mut invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let ToolPayload::Custom { input } = &invocation.payload else {
                return Err(FunctionCallError::RespondToModel(
                    "Raw script execution requires freeform source.".to_string(),
                ));
            };
            let metadata = parser::parse(input)?;
            if metadata.environment_id.is_some() && !self.options.include_environment_id {
                return Err(FunctionCallError::RespondToModel(
                    "environment_id is unavailable on this execution surface.".to_string(),
                ));
            }
            if metadata.background && !self.resumable {
                return Err(FunctionCallError::RespondToModel(
                    "Background execution is disabled by the session's process policy.".to_string(),
                ));
            }
            let initial_wait = if metadata.background {
                crate::tools::fence::Prerequisite::detached();
                InitialWait::Yield(MIN_YIELD_TIME_MS)
            } else {
                InitialWait::Exit
            };
            let mut arguments = serde_json::to_value(metadata)
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            arguments["cmd"] = input.clone().into();
            arguments["login"] = false.into();
            arguments["tty"] = false.into();
            invocation.payload = ToolPayload::Function {
                arguments: arguments.to_string(),
            };
            let handler = if self.resumable {
                ExecCommandHandler::new(self.options)
            } else {
                ExecCommandHandler::one_shot(self.options)
            };
            Box::pin(handler.handle_prepared(
                invocation,
                PreparedExec::Script {
                    language: self.language,
                    initial_wait,
                },
            ))
            .await
        })
    }
}

impl CoreToolRuntime for ScriptExecHandler {
    fn environments_for_input(
        &self,
        invocation: &ToolInvocation,
    ) -> Result<ToolEnvironments, FunctionCallError> {
        let ToolPayload::Custom { input } = &invocation.payload else {
            return Err(FunctionCallError::RespondToModel(
                "Raw script execution requires freeform source.".to_string(),
            ));
        };
        let metadata = parser::parse(input)?;
        if metadata.environment_id.is_some() && !self.options.include_environment_id {
            return Err(FunctionCallError::RespondToModel(
                "environment_id is unavailable on this execution surface.".to_string(),
            ));
        }
        resolve_tool_environment(
            &invocation.step_context.environments,
            metadata.environment_id.as_deref(),
        )
        .map(ToolEnvironments::single_executor)
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Custom { .. })
    }

    fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
        let ToolPayload::Custom { input } = &invocation.payload else {
            return None;
        };
        parser::parse(input).ok()?;
        Some(PreToolUsePayload {
            tool_name: invocation.tool_name.clone(),
            tool_input: serde_json::json!({ "command": self.language.review_command(input).ok()? }),
        })
    }

    fn pre_tool_use_cwd(
        &self,
        invocation: &ToolInvocation,
    ) -> Result<Option<codex_utils_path_uri::PathUri>, FunctionCallError> {
        let ToolPayload::Custom { input } = &invocation.payload else {
            return Ok(None);
        };
        let metadata = parser::parse(input)?;
        let environment = crate::tools::handlers::selected_executor(
            &invocation.environments,
            &invocation.step_context.environments,
        )?;
        let cwd = metadata
            .workdir
            .as_deref()
            .map_or_else(
                || Ok(environment.cwd().clone()),
                |workdir| environment.cwd().join(workdir),
            )
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        Ok((!environment.environment().is_remote()).then_some(cwd))
    }

    fn with_updated_hook_input(
        &self,
        invocation: ToolInvocation,
        updated_input: serde_json::Value,
    ) -> Result<ToolInvocation, FunctionCallError> {
        let original = self
            .pre_tool_use_payload(&invocation)
            .map(|payload| payload.tool_input);
        if original.as_ref() != Some(&updated_input) {
            return Err(FunctionCallError::RespondToModel("Raw script hook input is immutable; interpreter and source must match the reviewed request.".to_string()));
        }
        Ok(invocation)
    }

    fn post_tool_use_payload(
        &self,
        invocation: &ToolInvocation,
        result: &dyn ToolOutput,
    ) -> Option<PostToolUsePayload> {
        let pre = self.pre_tool_use_payload(invocation)?;
        result.post_tool_use_payload(&pre.tool_name, &invocation.call_id, pre.tool_input)
    }
}

pub(crate) enum PreparedExec {
    Shell,
    Script {
        language: ScriptLanguage,
        initial_wait: InitialWait,
    },
}

pub(crate) fn is_process_continuation(registry: &ToolRegistry, tool_name: &ToolName) -> bool {
    tool_name == &ToolName::plain("write_stdin")
        && registry.entries().any(|tool| {
            tool.exposure == ToolExposure::DirectModelOnly
                && [ScriptLanguage::Python, ScriptLanguage::Bash]
                    .iter()
                    .any(|language| tool.runtime.tool_name() == ToolName::plain(language.name()))
        })
}
