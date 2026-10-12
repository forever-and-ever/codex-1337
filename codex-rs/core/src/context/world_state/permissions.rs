//! Tracks model-visible permission instructions.

use super::PreviousSectionState;
use super::SectionTransition;
use super::WorldStateHash;
use super::WorldStateSection;
use super::WorldStateUpdate;
use crate::context::ContextualUserFragment;
use crate::context::PermissionsInstructions;
use codex_prompts::ApprovalPromptContext;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemSandboxPolicyContext;
use codex_protocol::protocol::AskForApproval;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;

/// Permission instructions currently visible to the model.
#[derive(Clone, Debug)]
pub(crate) struct PermissionsState {
    snapshot: PermissionsSnapshot,
    instructions: PermissionsInstructions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum PermissionsSnapshot {
    Current { instructions: WorldStateHash },
    Legacy(WorldStateHash),
}

impl PermissionsState {
    pub(crate) fn new(
        permission_profile: &PermissionProfile,
        approval_policy: AskForApproval,
        approval_context: ApprovalPromptContext<'_>,
        cwd: &Path,
        paths: Option<&FileSystemSandboxPolicyContext<'_>>,
        exec_permission_approvals_enabled: bool,
        request_permissions_tool_enabled: bool,
    ) -> Self {
        let instructions = PermissionsInstructions::from_permission_profile(
            permission_profile,
            approval_policy,
            approval_context,
            cwd,
            paths,
            exec_permission_approvals_enabled,
            request_permissions_tool_enabled,
        );
        let snapshot = PermissionsSnapshot::Current {
            instructions: WorldStateHash::from_fragment(&instructions),
        };
        Self {
            snapshot,
            instructions,
        }
    }
}

impl WorldStateSection for PermissionsState {
    const ID: &'static str = "permissions";
    type Snapshot = PermissionsSnapshot;

    fn matches_legacy_fragment(role: &str, text: &str) -> bool {
        role == "developer" && PermissionsInstructions::matches_text(text)
    }

    fn has_retained_fragment_matcher() -> bool {
        true
    }

    fn matches_retained_fragment(role: &str, text: &str) -> bool {
        Self::matches_legacy_fragment(role, text)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> SectionTransition<Self::Snapshot> {
        let current = self.snapshot.clone();
        match (previous, &self.snapshot) {
            (
                PreviousSectionState::Known(PermissionsSnapshot::Current {
                    instructions: previous_instructions,
                }),
                PermissionsSnapshot::Current { instructions },
            ) if previous_instructions == instructions => return (None, Vec::new()),
            (
                PreviousSectionState::Known(PermissionsSnapshot::Legacy(previous)),
                PermissionsSnapshot::Current { instructions },
            ) if previous == instructions => return (Some(current), Vec::new()),
            _ => {}
        }

        (
            Some(current),
            vec![WorldStateUpdate::fragment(self.instructions.clone())],
        )
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
