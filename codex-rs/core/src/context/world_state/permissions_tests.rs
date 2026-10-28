use super::*;
use crate::context::world_state::test_support::render_section_cases;
use codex_models_manager::model_info::model_info_from_slug;
use codex_prompts::ResolvedModelMessages;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::models::ContentItem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ApprovalMessages;
use codex_protocol::openai_models::ModelMessages;
use codex_protocol::openai_models::PermissionMessages;
use codex_protocol::protocol::AskForApproval;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::Path;

#[test]
fn snapshots() {
    use PreviousSectionState::Absent;
    use PreviousSectionState::Known;
    use PreviousSectionState::Unknown;

    let read_only = permissions_state(PermissionProfile::read_only(), AskForApproval::OnRequest);
    let full_access = permissions_state(PermissionProfile::Disabled, AskForApproval::OnRequest);
    let never_ask = permissions_state(PermissionProfile::read_only(), AskForApproval::Never);

    insta::assert_snapshot!(render_section_cases(&[
        (Absent, Absent),
        (Absent, Known(&read_only)),
        (Known(&read_only), Known(&read_only)),
        (Known(&read_only), Known(&full_access)),
        (Known(&read_only), Known(&never_ask)),
        (Unknown, Known(&read_only)),
    ]));
}

#[test]
fn legacy_snapshot_deserializes_and_only_suppresses_matching_full_permissions() {
    use super::super::WorldState;
    use super::super::WorldStateSnapshot;

    let read_only = permissions_state(PermissionProfile::read_only(), AskForApproval::OnRequest);
    let full_access = permissions_state(PermissionProfile::Disabled, AskForApproval::OnRequest);
    let current_hash = WorldStateHash::from_fragment(&full_access.instructions);
    let stale_hash = WorldStateHash::from_fragment(&read_only.instructions);
    let expected_permissions = full_access.instructions.render();
    let retained = ContextualUserFragment::into(full_access.instructions.clone());
    let mut world_state = WorldState::default();
    world_state.add_section(full_access);

    // Both persisted schemas must suppress unchanged instructions without retaining prefixes.
    for (matching, stale) in [
        (json!(current_hash), json!(stale_hash)),
        (
            json!({ "instructions": current_hash, "approved_command_prefixes": [["touch"]] }),
            json!({ "instructions": stale_hash, "approved_command_prefixes": [["touch"]] }),
        ),
    ] {
        let matching: WorldStateSnapshot =
            serde_json::from_value(json!({ "permissions": matching }))
                .expect("persisted world-state snapshot should deserialize");
        let stale: WorldStateSnapshot = serde_json::from_value(json!({ "permissions": stale }))
            .expect("persisted world-state snapshot should deserialize");
        assert!(
            world_state
                .render_history_fragment_diff(Some(&matching), std::slice::from_ref(&retained))
                .1
                .is_empty()
        );
        assert_eq!(
            world_state
                .render_history_fragment_diff(Some(&stale), std::slice::from_ref(&retained))
                .1
                .into_iter()
                .map(|fragment| fragment.render())
                .collect::<Vec<_>>(),
            vec![expected_permissions.clone()]
        );
    }
}

#[test]
fn persisted_permissions_are_detected_inside_bundled_developer_messages() {
    let state = permissions_state(PermissionProfile::read_only(), AskForApproval::OnRequest);
    let retained = ContextualUserFragment::into(state.instructions.clone());
    let mut world_state = super::super::WorldState::default();
    world_state.add_section(state);
    let snapshot = world_state.render_full().0;
    let mut bundled_retained = retained.clone();
    let ResponseItem::Message { content, .. } = &mut bundled_retained else {
        panic!("permissions should render as a message");
    };
    content.insert(
        0,
        ContentItem::InputText {
            text: "Other developer instructions.".to_string(),
        },
    );

    assert_eq!(
        world_state
            .render_history_fragment_diff(/*previous*/ None, std::slice::from_ref(&retained))
            .1
            .len(),
        1,
    );
    assert_eq!(
        world_state
            .render_history_fragment_diff(Some(&snapshot), &[])
            .1
            .len(),
        1,
    );
    assert!(
        world_state
            .render_history_fragment_diff(Some(&snapshot), &[bundled_retained])
            .1
            .is_empty()
    );
}

fn permissions_state(
    permission_profile: PermissionProfile,
    approval_policy: AskForApproval,
) -> PermissionsState {
    let approval_messages = ApprovalMessages {
        on_request: Some("Ask for approval.".to_string()),
        on_request_auto_review: None,
        never: None,
        unless_trusted: None,
    };
    let permission_messages = PermissionMessages {
        danger_full_access: Some("Full access.".to_string()),
        workspace_write: Some("Workspace write.".to_string()),
        read_only: Some("Read only.".to_string()),
        approved_prefixes: None,
    };
    let mut model = model_info_from_slug("test-model");
    model.model_messages = Some(ModelMessages {
        approvals: Some(approval_messages),
        permissions: Some(permission_messages),
        ..Default::default()
    });
    let model_messages = ResolvedModelMessages::from_model(&model);
    PermissionsState::new(
        &permission_profile,
        approval_policy,
        ApprovalPromptContext::new(ApprovalsReviewer::User, model_messages),
        Path::new("/workspace"),
        /*paths*/ None,
        /*exec_permission_approvals_enabled*/ false,
        /*request_permissions_tool_enabled*/ false,
    )
}
