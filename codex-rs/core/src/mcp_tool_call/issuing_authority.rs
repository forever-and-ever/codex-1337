//! An MCP call answers to the turn step that issued it. Thread settings saved
//! during a turn govern later turns, as they do for every other tool.

use codex_mcp::PreparedMcpCall;
use codex_protocol::models::PermissionProfile;

use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnEnvironment;

/// The issuing step's permissions over the call's server. A server outside the
/// step's environments receives the least authority.
pub(super) fn issuing_permission_profile(
    step: &StepContext,
    call: &PreparedMcpCall,
) -> PermissionProfile {
    let thread_profile = step.turn.config.permissions.effective_permission_profile();
    let environment_profile = step
        .environments
        .turn_environments()
        .find(|environment| environment.selection.environment_id == call.server_environment_id())
        .map(TurnEnvironment::permission_profile_with_workspace_roots);
    call.config()
        .server_permission_profile(call.server_key(), &thread_profile, |_| {
            environment_profile.as_ref()
        })
        .cloned()
        .unwrap_or_default()
}
