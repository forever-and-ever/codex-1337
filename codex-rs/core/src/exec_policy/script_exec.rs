use super::ExecApprovalRequest;
use super::ExecPolicyManager;
use super::commands_for_exec_policy_for_platform;
use crate::tools::sandboxing::ExecApprovalRequirement;
use codex_shell_command::is_dangerous_command::DangerousCommandPlatform;

impl ExecPolicyManager {
    pub(crate) async fn create_exec_approval_requirement_for_script(
        &self,
        request: ExecApprovalRequest<'_>,
        platform: DangerousCommandPlatform,
    ) -> ExecApprovalRequirement {
        let mut commands = commands_for_exec_policy_for_platform(request.command, platform);
        // Shell parsing may add restrictions, but inner-command rules cannot grant
        // an interpreter resolved through the executor's PATH a sandbox bypass.
        if commands.commands != [request.command.to_vec()] {
            commands.commands.insert(0, request.command.to_vec());
        }
        match self
            .create_exec_approval_requirement_for_parsed_commands(request, commands, platform)
            .await
        {
            ExecApprovalRequirement::NeedsApproval { reason, .. } => {
                ExecApprovalRequirement::NeedsApproval {
                    reason,
                    proposed_execpolicy_amendment: None,
                }
            }
            ExecApprovalRequirement::Skip { bypass_sandbox, .. } => ExecApprovalRequirement::Skip {
                bypass_sandbox,
                proposed_execpolicy_amendment: None,
            },
            human @ ExecApprovalRequirement::NeedsHumanApproval { .. } => human,
            forbidden @ ExecApprovalRequirement::Forbidden { .. } => forbidden,
        }
    }
}
