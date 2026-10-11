use crate::shell::ShellInvocation;
use codex_tools::UnifiedExecShellMode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScriptLanguage {
    Python,
    Bash,
}

impl ScriptLanguage {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Bash => "bash",
        }
    }

    pub(crate) fn command(self, source: &str) -> Vec<String> {
        let arguments: &[&str] = match self {
            Self::Python => &["python3", "-u", "-c", source],
            Self::Bash => &["bash", "-c", source],
        };
        arguments
            .iter()
            .map(|argument| (*argument).to_string())
            .collect()
    }

    pub(crate) fn review_command(
        self,
        source: &str,
    ) -> Result<String, crate::function_tool::FunctionCallError> {
        let command = self.command(source);
        shlex::try_join(command.iter().map(String::as_str)).map_err(|err| {
            crate::function_tool::FunctionCallError::RespondToModel(format!(
                "Invalid script command: {err}"
            ))
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ExecutionMode {
    Shell {
        invocation: ShellInvocation,
        shell_mode: UnifiedExecShellMode,
    },
    Script(ScriptLanguage),
}

impl ExecutionMode {
    pub(crate) async fn approval_requirement(
        &self,
        policy: &crate::exec_policy::ExecPolicyManager,
        request: crate::exec_policy::ExecApprovalRequest<'_>,
        configured_shell: &crate::shell::Shell,
        platform: codex_shell_command::is_dangerous_command::DangerousCommandPlatform,
    ) -> crate::tools::sandboxing::ExecApprovalRequirement {
        match self {
            Self::Shell { shell_mode, .. } => {
                policy
                    .create_exec_approval_requirement_for_shell(
                        request,
                        configured_shell,
                        shell_mode,
                        platform,
                    )
                    .await
            }
            Self::Script(_) => {
                policy
                    .create_exec_approval_requirement_for_script(request, platform)
                    .await
            }
        }
    }

    pub(crate) fn shell(&self) -> Option<(&ShellInvocation, &UnifiedExecShellMode)> {
        match self {
            Self::Shell {
                invocation,
                shell_mode,
            } => Some((invocation, shell_mode)),
            Self::Script(_) => None,
        }
    }

    pub(crate) fn tool_name(&self) -> &'static str {
        match self {
            Self::Shell { .. } => "exec_command",
            Self::Script(language) => language.name(),
        }
    }
}
