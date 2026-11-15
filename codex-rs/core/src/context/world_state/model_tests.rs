use super::*;
use crate::context::world_state::test_support::FragmentSectionTestExt as _;
use pretty_assertions::assert_eq;

#[test]
fn model_change_renders_when_persisted_or_inferred_from_previous_turn() {
    let state = ModelInstructionsState::new("gpt-new", Some("gpt-old"), "instructions".into());
    let previous = ModelInstructionsSnapshot::ModelOnly("gpt-old".to_string());
    let previous_incremental = serde_json::from_value(serde_json::json!({
        "model": "gpt-new",
        "instructions": WorldStateHash::from_fragment(&BaseInstructionsFragment("instructions".into())),
    }))
    .unwrap();

    for previous in [
        PreviousSectionState::Known(&previous),
        PreviousSectionState::Known(&previous_incremental),
        PreviousSectionState::Unknown,
        PreviousSectionState::Absent,
    ] {
        assert_eq!(
            state
                .render_fragment_diff(previous)
                .1
                .expect("model change should render")
                .markers(),
            ModelSwitchInstructions::type_markers()
        );
    }
}

#[test]
fn unchanged_model_does_not_render() {
    let state = ModelInstructionsState::new("gpt-test", Some("gpt-test"), "instructions".into());
    let previous = ModelInstructionsSnapshot::ModelOnly("gpt-test".to_string());

    let (snapshot, fragment) = state.render_fragment_diff(PreviousSectionState::Known(&previous));
    assert_eq!(
        serde_json::to_value(snapshot).unwrap(),
        serde_json::json!("gpt-test")
    );
    assert!(fragment.is_none());
    assert!(
        state
            .render_fragment_diff(PreviousSectionState::Absent)
            .1
            .is_none()
    );
}

#[test]
fn legacy_model_snapshot_keeps_its_window_until_model_change_or_compaction() {
    let legacy: ModelInstructionsSnapshot = serde_json::from_str("\"gpt-old\"").unwrap();
    let same_model = ModelInstructionsState::for_incremental_tools(
        "gpt-old",
        "new prompt".into(),
        /*suppress_model_change_announcements*/ false,
    );
    let (snapshot, updates) = same_model.render_diff(PreviousSectionState::Known(&legacy));
    assert!(snapshot.is_none());
    assert!(updates.is_empty());

    let new_model = ModelInstructionsState::for_incremental_tools(
        "gpt-new",
        "new prompt".into(),
        /*suppress_model_change_announcements*/ false,
    );
    let (snapshot, updates) = new_model.render_diff(PreviousSectionState::Known(&legacy));
    assert!(snapshot.is_some());
    assert_eq!(updates.len(), 1);

    for previous in [PreviousSectionState::Absent, PreviousSectionState::Unknown] {
        let (snapshot, updates) = same_model.render_diff(previous);
        assert_eq!(
            crate::context_manager::updates::merge_world_state_updates(updates),
            vec![ContextualUserFragment::into(BaseInstructionsFragment(
                "new prompt".into()
            ))]
        );
        let snapshot = snapshot.unwrap();
        assert!(
            same_model
                .render_diff(PreviousSectionState::Known(&snapshot))
                .1
                .is_empty()
        );
    }

    // Both modes retain one dedicated prefix and amend it without rewriting history.
    for incremental in [false, true] {
        let state = |text: &str| {
            let mut state = crate::context::world_state::WorldState::default();
            if incremental {
                state.add_instruction_section_with_identity(
                    ModelInstructionsState::for_incremental_tools(
                        "gpt-old",
                        text.to_string(),
                        false,
                    ),
                    "base_instructions",
                );
            } else {
                state.add_instruction_section(
                    crate::context::world_state::instruction_base::BaseInstructionsState(
                        text.to_string(),
                    ),
                );
            }
            state
        };
        let (previous, initial) = state("initial base").render_full();
        let history = crate::context_manager::updates::merge_world_state_updates(initial);
        assert_eq!(
            history,
            vec![ContextualUserFragment::into(BaseInstructionsFragment(
                "initial base".into()
            ))]
        );
        let original = history.clone();
        let current = state("updated base");
        let (snapshot, updates) = current.render_history_diff(Some(&previous), &history);
        let updates = crate::context_manager::updates::merge_world_state_updates(updates);
        assert_eq!(updates.len(), 1);
        let mut amendment = updates[0].clone();
        let parts = codex_context_fragments::to_annotated_content(&mut amendment).unwrap();
        assert_eq!(
            parts[0].kind().as_str(),
            "context.instruction_diff/base_instructions"
        );
        let mut continued = history;
        continued.extend(updates);
        assert!(
            current
                .render_history_diff(Some(&snapshot), &continued)
                .1
                .is_empty()
        );
        assert_eq!(&continued[..original.len()], original.as_slice());
        let projected = snapshot.materialize_instructions(
            &snapshot,
            continued
                .into_iter()
                .map(codex_history::ResponseItemEnvelope::new)
                .collect(),
        );
        assert_eq!(
            projected
                .into_iter()
                .map(codex_history::ResponseItemEnvelope::into_item)
                .collect::<Vec<_>>(),
            vec![ContextualUserFragment::into(BaseInstructionsFragment(
                "updated base".into()
            ))]
        );
        let (_, reannounced) = current.render_history_diff(Some(&snapshot), &[]);
        assert_eq!(
            crate::context_manager::updates::merge_world_state_updates(reannounced),
            vec![ContextualUserFragment::into(BaseInstructionsFragment(
                "updated base".into()
            ))]
        );
    }

    // A model-name transition is independent of whether its base instructions changed.
    let incremental = |model: &str, text: &str| {
        let mut state = crate::context::world_state::WorldState::default();
        state.add_instruction_section_with_identity(
            ModelInstructionsState::for_incremental_tools(model, text.to_string(), false),
            "base_instructions",
        );
        state.add_section(crate::context::world_state::ModelIdentityState {
            model: model.to_string(),
            suppress_model_change_announcements: false,
        });
        state
    };
    let (previous, initial) = incremental("gpt-old", "initial base").render_full();
    let history = crate::context_manager::updates::merge_world_state_updates(initial);
    assert_eq!(
        history,
        vec![ContextualUserFragment::into(BaseInstructionsFragment(
            "initial base".into()
        ))]
    );
    let identity = ContextualUserFragment::into(crate::context::ModelInstructions {
        model: Some("gpt-new".into()),
        instructions: None,
    });
    for text in ["initial base", "updated base"] {
        let current = incremental("gpt-new", text);
        let (snapshot, updates) = current.render_history_diff(Some(&previous), &history);
        assert!(
            serde_json::to_value(&snapshot)
                .unwrap()
                .get("model_identity")
                .is_none()
        );
        let updates = crate::context_manager::updates::merge_world_state_updates(updates);
        assert_eq!(updates.iter().filter(|item| *item == &identity).count(), 1);
        assert_eq!(updates.len(), if text == "initial base" { 1 } else { 2 });
        if text != "initial base" {
            let mut amendment = updates
                .iter()
                .find(|item| *item != &identity)
                .unwrap()
                .clone();
            let parts = codex_context_fragments::to_annotated_content(&mut amendment).unwrap();
            assert_eq!(
                parts[0].kind().as_str(),
                "context.instruction_diff/base_instructions"
            );
        }
        let mut continued = history.clone();
        continued.extend(updates);
        assert!(
            current
                .render_history_diff(Some(&snapshot), &continued)
                .1
                .is_empty()
        );
        assert_eq!(
            crate::context_manager::updates::merge_world_state_updates(current.render_full().1),
            vec![ContextualUserFragment::into(BaseInstructionsFragment(
                text.into()
            ))]
        );
    }
}

#[test]
fn suppressed_model_changes_advance_snapshot_without_repeated_updates() {
    let initial = ModelInstructionsState::for_incremental_tools(
        "gpt-old",
        "instructions".into(),
        /*suppress_model_change_announcements*/ true,
    );
    let (snapshot, _) = initial.render_diff(PreviousSectionState::Absent);
    let previous = snapshot.unwrap();
    let switched = ModelInstructionsState::for_incremental_tools(
        "gpt-new",
        "instructions".into(),
        /*suppress_model_change_announcements*/ true,
    );
    let (snapshot, updates) = switched.render_diff(PreviousSectionState::Known(&previous));
    assert!(updates.is_empty());
    let snapshot = snapshot.unwrap();
    assert_eq!(snapshot, switched.snapshot);
    let (unchanged, updates) = switched.render_diff(PreviousSectionState::Known(&snapshot));
    assert!(unchanged.is_none());
    assert!(updates.is_empty());
    // The fork's independent identity section must obey the same announcement policy.
    let mut state = crate::context::world_state::WorldState::default();
    state.add_instruction_section_with_identity(initial, "base_instructions");
    state.add_section(crate::context::world_state::ModelIdentityState {
        model: "gpt-old".into(),
        suppress_model_change_announcements: true,
    });
    let (previous, initial) = state.render_full();
    let history = crate::context_manager::updates::merge_world_state_updates(initial);
    let mut state = crate::context::world_state::WorldState::default();
    state.add_instruction_section_with_identity(switched, "base_instructions");
    state.add_section(crate::context::world_state::ModelIdentityState {
        model: "gpt-new".into(),
        suppress_model_change_announcements: true,
    });
    let (snapshot, updates) = state.render_history_diff(Some(&previous), &history);
    assert!(updates.is_empty());
    assert!(
        state
            .render_history_diff(Some(&snapshot), &history)
            .1
            .is_empty()
    );
}
