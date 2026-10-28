//! Canonical request construction shared by background submission and focused facade tests.

use super::AbsolutePathBuf;
use super::AskForApproval;
use super::PathBuf;
use super::Result;
use super::ThreadId;
use super::TurnPermissionsOverride;
use super::UserInput;
use super::turn_permissions_overrides;
use codex_app_server_protocol::TurnStartParams;

#[expect(
    clippy::option_option,
    reason = "Wire patch distinguishes keeping, clearing, and setting the service tier"
)]
pub(crate) fn turn_start_params(
    thread_id: ThreadId,
    client_user_message_id: String,
    items: Vec<UserInput>,
    cwd: PathBuf,
    approval_policy: Option<AskForApproval>,
    approvals_reviewer: Option<codex_app_server_protocol::ApprovalsReviewer>,
    permissions_override: TurnPermissionsOverride,
    workspace_roots: &[AbsolutePathBuf],
    model: String,
    effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    summary: Option<codex_protocol::config_types::ReasoningSummary>,
    service_tier: Option<Option<String>>,
    collaboration_mode: Option<codex_protocol::config_types::CollaborationMode>,
    output_schema: Option<serde_json::Value>,
    cyber_access_program: Option<codex_app_server_protocol::CyberAccessProgram>,
) -> Result<TurnStartParams> {
    let (sandbox_policy, permissions) =
        turn_permissions_overrides(permissions_override, cwd.as_path())?;
    Ok(TurnStartParams {
        disabled_plugin_ids: None,
        thread_id: thread_id.to_string(),
        turn_trigger: Some("user".to_string()),
        parent_turn_id: None,
        root_turn_id: None,
        client_user_message_id: Some(client_user_message_id),
        input: items,
        tool_output: None,
        responsesapi_client_metadata: None,
        responsesapi_client_metadata_patch: None,
        additional_context: None,
        environments: None,
        cwd: Some(cwd),
        runtime_workspace_roots: Some(workspace_roots.to_vec()),
        approval_policy,
        approvals_reviewer,
        sandbox_policy,
        permissions,
        model: Some(model),
        service_tier,
        service_tier_for_turn: None,
        effort,
        summary,
        personality: None,
        output_schema,
        collaboration_mode,
        multi_agent_mode: None,
        cyber_access_program,
    })
}
