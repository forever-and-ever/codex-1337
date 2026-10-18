use crate::shell::ShellInvocation;

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
    Shell { invocation: ShellInvocation },
    Script,
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
            Self::Shell { .. } => {
                policy
                    .create_exec_approval_requirement_for_shell(request, configured_shell, platform)
                    .await
            }
            Self::Script => {
                policy
                    .create_exec_approval_requirement_for_script(request, platform)
                    .await
            }
        }
    }

    pub(crate) fn shell(&self) -> Option<&ShellInvocation> {
        match self {
            Self::Shell { invocation } => Some(invocation),
            Self::Script => None,
        }
    }
}
