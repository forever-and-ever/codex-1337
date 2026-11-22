//! Tracks model-visible permission instructions.

use super::PreviousSectionState;
use super::SectionTransition;
use super::WorldStateContextFragment;
use super::WorldStateHash;
use super::WorldStateSection;
use super::WorldStateUpdate;
use crate::context::ContextualUserFragment;
use crate::context::PermissionsInstructions;
use crate::context::environment_context::push_xml_escaped_text;
use codex_extension_api::RenderedWorldStateFragment;
use codex_prompts::ApprovalPromptContext;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::input_text;
use codex_protocol::permissions::FileSystemSandboxPolicyContext;
use codex_protocol::protocol::AskForApproval;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Permission instructions currently visible to the model.
#[derive(Clone, Debug)]
pub(crate) struct PermissionsState {
    snapshot: PermissionsSnapshot,
    // None describes the thread defaults.
    descriptions: BTreeMap<Option<String>, PermissionDescription>,
}

/// One permission description and its comparison value.
#[derive(Clone, Debug)]
pub(crate) struct PermissionDescription {
    instructions: PermissionsInstructions,
    fingerprint: WorldStateHash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum PermissionsSnapshot {
    Scoped {
        environments: BTreeMap<String, WorldStateHash>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_defaults: Option<WorldStateHash>,
    },
    Current {
        instructions: WorldStateHash,
    },
    Legacy(WorldStateHash),
}

impl PermissionDescription {
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
        let fingerprint = WorldStateHash::from_fragment(&instructions);
        Self {
            instructions,
            fingerprint,
        }
    }
}

impl PermissionsState {
    pub(crate) fn new(
        descriptions: BTreeMap<Option<String>, PermissionDescription>,
        per_environment: bool,
    ) -> Self {
        let snapshot = if per_environment {
            PermissionsSnapshot::Scoped {
                environments: descriptions
                    .iter()
                    .filter_map(|(id, description)| {
                        Some((id.clone()?, description.fingerprint.clone()))
                    })
                    .collect(),
                thread_defaults: descriptions
                    .get(&None)
                    .map(|description| description.fingerprint.clone()),
            }
        } else {
            PermissionsSnapshot::Current {
                instructions: descriptions[&None].fingerprint.clone(),
            }
        };
        Self {
            snapshot,
            descriptions,
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
        if let PreviousSectionState::Known(previous) = &previous
            && *previous == &self.snapshot
        {
            return (None, Vec::new());
        }
        let current = self.snapshot.clone();
        if matches!(self.snapshot, PermissionsSnapshot::Scoped { .. }) {
            let mut updates = Vec::new();
            let mut previous_descriptions = BTreeMap::new();
            if let PreviousSectionState::Known(PermissionsSnapshot::Scoped {
                environments,
                thread_defaults,
            }) = previous
            {
                previous_descriptions.extend(
                    environments
                        .iter()
                        .map(|(id, hash)| (Some(id.clone()), hash)),
                );
                previous_descriptions.extend(thread_defaults.as_ref().map(|hash| (None, hash)));
            }
            // An environment can remain selected but stop being ready. Withdraw its old
            // description even when the separate environment-context section is disabled.
            for id in previous_descriptions
                .keys()
                .filter(|id| !self.descriptions.contains_key(*id))
            {
                updates.push(scoped_permissions_update(
                    id.as_deref(),
                    /*instructions*/ None,
                ));
            }
            for (id, description) in &self.descriptions {
                if previous_descriptions.get(id).copied() != Some(&description.fingerprint) {
                    updates.push(scoped_permissions_update(
                        id.as_deref(),
                        Some(&description.instructions),
                    ));
                }
            }
            return (Some(current), updates);
        }
        let instructions = &self.descriptions[&None].instructions;
        if let PreviousSectionState::Known(PermissionsSnapshot::Scoped {
            environments,
            thread_defaults,
            ..
        }) = &previous
        {
            let mut updates = environments
                .keys()
                .map(|id| Some(id.as_str()))
                .chain(thread_defaults.as_ref().map(|_| None))
                .map(|id| scoped_permissions_update(id, /*instructions*/ None))
                .collect::<Vec<_>>();
            updates.push(WorldStateUpdate::fragment(instructions.clone()));
            return (Some(current), updates);
        }
        match (previous, &self.snapshot) {
            (
                PreviousSectionState::Known(PermissionsSnapshot::Current {
                    instructions: previous_instructions,
                }),
                PermissionsSnapshot::Current { instructions },
            ) if previous_instructions == instructions => return (None, Vec::new()),
            (
                PreviousSectionState::Known(PermissionsSnapshot::Legacy(previous)),
                PermissionsSnapshot::Current { .. },
            ) if previous == &WorldStateHash::from_fragment(instructions) => {
                return (Some(current), Vec::new());
            }
            _ => {}
        }

        (
            Some(current),
            vec![WorldStateUpdate::fragment(instructions.clone())],
        )
    }
}

/// Labels an environment's rules, or thread defaults when no environment ID is supplied.
/// Missing instructions withdraw the previous description for that scope.
fn scoped_permissions_update(
    environment_id: Option<&str>,
    instructions: Option<&PermissionsInstructions>,
) -> WorldStateUpdate {
    let mut text = String::new();
    let end = if let Some(id) = environment_id {
        text.push_str("<environment id=\"");
        push_xml_escaped_text(&mut text, id);
        text.push_str("\">\n");
        "</environment>"
    } else {
        text.push_str("<thread_defaults>\n");
        "</thread_defaults>"
    };
    if let Some(instructions) = instructions {
        let ContentItem::InputText { text: body } = instructions.body() else {
            unreachable!("permission instructions are text")
        };
        text.push_str(&body);
    } else {
        text.push_str("The previous permission description for this scope is no longer current.\n");
    }
    text.push_str(end);
    WorldStateUpdate::fragment(WorldStateContextFragment {
        fragment: RenderedWorldStateFragment::new(
            "developer",
            PermissionsInstructions::type_markers(),
            input_text(text),
        ),
        content_kind: ContentItemKind("permissions.instructions".to_string()),
    })
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
