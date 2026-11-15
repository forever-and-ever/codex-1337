use super::*;
use codex_context_fragments::AnnotatedContent;
use codex_protocol::models::AudioReference;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::input_text;
use codex_protocol_test_support::ContentItemTestExt;
use pretty_assertions::assert_eq;
use pretty_assertions::assert_ne;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Clone, Deserialize, Serialize)]
struct TestSection {
    value: String,
    optional: Option<String>,
    array: Vec<Value>,
}

impl WorldStateSection for TestSection {
    const ID: &'static str = "test";
    type Snapshot = Self;

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> SectionTransition<Self::Snapshot> {
        let current = self.clone();
        let fragment: Option<super::WorldStateFragment> = match previous {
            PreviousSectionState::Known(previous) if self.value != previous.value => {
                Some(Box::new(TestFragment(self.value.clone())))
            }
            PreviousSectionState::Unknown => Some(Box::new(TestFragment("unknown".to_string()))),
            PreviousSectionState::Absent => Some(Box::new(TestFragment(self.value.clone()))),
            PreviousSectionState::Known(_) => None,
        };
        (
            Some(current),
            WorldStateUpdate::optional_boxed_fragment(fragment),
        )
    }
}

struct TestFragment(String);

impl ContextualUserFragment for TestFragment {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("generic.test".to_string())
    }

    fn content_metadata(&self) -> codex_protocol::models::ContentItemMetadata {
        codex_protocol::models::ContentItemMetadata::skills()
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> ContentItem {
        input_text(self.0.clone())
    }
}

#[test]
fn world_state_hash_normalizes_crlf_line_endings() {
    assert_eq!(
        WorldStateHash::from_fragment(&TestFragment("line one\r\nline two".to_string())),
        WorldStateHash::from_fragment(&TestFragment("line one\nline two".to_string())),
    );
}

#[test]
fn world_state_hash_tracks_typed_content_and_role() {
    let hash = |role, audio_url: &str| {
        WorldStateHash::from_fragment(&WorldStateContextFragment {
            fragment: RenderedWorldStateFragment::new(
                role,
                ("", ""),
                ContentItem::InputAudio {
                    audio: AudioReference::Inline {
                        audio_url: audio_url.to_string(),
                    },
                },
            ),
            content_kind: ContentItemKind("generic.test".to_string()),
        })
    };

    assert_eq!(
        hash("developer", "audio-one"),
        hash("developer", "audio-one")
    );
    assert_ne!(
        hash("developer", "audio-one"),
        hash("developer", "audio-two")
    );
    assert_ne!(hash("developer", "audio-one"), hash("user", "audio-one"));
}

#[test]
fn world_state_json_hash_ignores_object_key_order() {
    let first: Value = serde_json::from_str(
        r#"{"type":"function","parameters":{"type":"object","properties":{"query":{"type":"string"}}}}"#,
    )
    .unwrap();
    let reordered: Value = serde_json::from_str(
        r#"{"parameters":{"properties":{"query":{"type":"string"}},"type":"object"},"type":"function"}"#,
    )
    .unwrap();
    assert_eq!(
        WorldStateHash::from_json(&first),
        WorldStateHash::from_json(&reordered),
    );
}

struct DuplicateTestSection;

impl WorldStateSection for DuplicateTestSection {
    const ID: &'static str = "test";
    type Snapshot = ();

    fn render_diff(
        &self,
        _previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> SectionTransition<Self::Snapshot> {
        (Some(()), Vec::new())
    }
}

#[test]
fn snapshot_uses_stable_section_ids_and_omits_null_fields() {
    let mut world_state = WorldState::default();
    world_state.add_section(TestSection {
        value: "current".to_string(),
        optional: None,
        array: vec![json!({"value": null})],
    });

    assert_eq!(
        serde_json::to_value(world_state.render_full().0).expect("serialize world-state snapshot"),
        json!({"test": {"value": "current", "array": [{"value": null}]}})
    );
}

#[test]
fn render_diff_restores_the_typed_section_snapshot() {
    let state = |value: &str| {
        let mut state = WorldState::default();
        state.add_instruction_section(TestSection {
            value: value.to_string(),
            optional: None,
            array: Vec::new(),
        });
        state
    };
    let before = state("before");
    let after = state("after");
    assert!(
        state("").render_full().1.is_empty(),
        "a blank section says nothing"
    );
    let (snapshot, initial) = before.render_full();
    let mut history = crate::context_manager::updates::merge_world_state_updates(initial);
    let original = history.clone();
    let (snapshot, amendment) = after.render_history_diff(Some(&snapshot), &history);
    let amendment = crate::context_manager::updates::merge_world_state_updates(amendment);
    assert_eq!(amendment.len(), 1);
    let ResponseItem::Message { content, .. } = &amendment[0] else {
        panic!("expected amendment");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected text");
    };
    assert!(text.contains("<instruction_diff>"));
    assert!(text.contains("-before"));
    assert!(text.contains("+after"));
    history.extend(amendment);
    let restored = WorldStateSnapshot::from(&snapshot.into_object());
    assert!(
        after
            .render_history_diff(Some(&restored), &history)
            .1
            .is_empty()
    );
    // Older builds persisted revisions without provenance; adopting it amends nothing.
    let mut legacy = restored.clone();
    legacy
        .sections
        .get_mut("codex.instruction/test")
        .and_then(|revision| revision["current"].as_object_mut())
        .expect("current instruction revision")
        .remove("metadata");
    let (adopted, updates) = after.render_history_diff(Some(&legacy), &history);
    assert!(updates.is_empty());
    assert_eq!(adopted, restored);

    // Replay must retain the complete patch ancestry; a surviving tip alone is not enough.
    let (_, replacement) = after.render_history_diff(Some(&restored), &history[1..]);
    let replacement = crate::context_manager::updates::merge_world_state_updates(replacement);
    let (fresh, full) = after.render_full();
    let full = crate::context_manager::updates::merge_world_state_updates(full);
    assert_eq!(replacement, full);
    let materialized = restored
        .materialize_instructions(
            &fresh,
            history
                .iter()
                .cloned()
                .map(codex_history::ResponseItemEnvelope::new)
                .collect(),
        )
        .into_iter()
        .map(codex_history::ResponseItemEnvelope::into_item)
        .collect::<Vec<_>>();
    assert_eq!(materialized, full);
    let mut projected = materialized[0].clone();
    let parts = codex_context_fragments::to_annotated_content(&mut projected).unwrap();
    assert_eq!(
        parts[0].clone().into_parts().2,
        codex_protocol::models::ContentItemMetadata::skills()
    );
    assert_eq!(history[0], original[0]);

    // Compaction edits host-owned instructions, never opaque provider state or
    // unannotated/partially annotated user evidence, even when its text matches.
    let checkpoint: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "native_compaction", "id": "cmp_signed", "provider": "anthropic",
        "content": "summary", "signature": "exact-signed+/block="
    }))
    .unwrap();
    let user: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "message", "role": "user",
        "content": [{"type": "input_text", "text": "before"}]
    }))
    .unwrap();
    let mut partial = original[0].clone();
    if let ResponseItem::Message { content, .. } = &mut partial {
        content.push(ContentItem::InputText {
            text: "user authorization".into(),
        });
    }
    let protected = vec![user, partial];
    let mut source = vec![checkpoint.clone()];
    source.extend(history.clone());
    source.extend(replacement);
    source.extend(protected.clone());
    let project = |snapshot: &WorldStateSnapshot, current: &WorldStateSnapshot| {
        snapshot
            .materialize_instructions(
                current,
                source
                    .iter()
                    .cloned()
                    .map(codex_history::ResponseItemEnvelope::new)
                    .collect(),
            )
            .into_iter()
            .map(codex_history::ResponseItemEnvelope::into_item)
            .collect::<Vec<_>>()
    };
    // A fresh ledger still recognizes earlier host-owned amendments by section identity.
    let expected = std::iter::once(checkpoint.clone())
        .chain(full)
        .chain(protected.clone())
        .collect::<Vec<_>>();
    assert_eq!(project(&fresh, &fresh), expected);
    let removed = WorldState::default().render_full().0;
    let expected = std::iter::once(checkpoint)
        .chain(protected)
        .collect::<Vec<_>>();
    assert_eq!(project(&restored, &removed), expected);
}

#[test]
fn extension_owned_section_uses_its_snapshot_and_renderer() {
    let mut world_state = WorldState::default();
    world_state.add_extension_section(WorldStateSectionContribution::new(
        "extension_test",
        |previous| {
            (
                Some(json!({"value": "after", "optional": null})),
                match previous {
                    PreviousWorldStateSection::Known(previous)
                        if previous == &json!({"value": "before"}) =>
                    {
                        Some(RenderedWorldStateFragment::new(
                            "developer",
                            ("<extension_test>", "</extension_test>"),
                            input_text("after".to_string()),
                        ))
                    }
                    PreviousWorldStateSection::Absent
                    | PreviousWorldStateSection::Unknown
                    | PreviousWorldStateSection::Known(_) => None,
                },
            )
        },
    ));
    let previous = WorldStateSnapshot {
        sections: BTreeMap::from([("extension_test".to_string(), json!({"value": "before"}))]),
        ..Default::default()
    };

    let rendered = world_state
        .render_history_fragment_diff(Some(&previous), &[])
        .1;

    assert_eq!(
        serde_json::to_value(world_state.render_full().0).expect("serialize world-state snapshot"),
        json!({"extension_test": {"value": "after"}})
    );
    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].role(), "developer");
    assert_eq!(
        rendered[0].render().expect_text(),
        "<extension_test>after</extension_test>"
    );
}

#[test]
fn extension_owned_section_preserves_typed_content_and_attribution() {
    let image = ContentItem::InputImage {
        image: codex_protocol::models::ImageReference::File {
            file_id: "extension-image".to_string(),
        },
        detail: None,
    };
    for (body, expected) in [
        (
            input_text("after".to_string()),
            input_text("<extension_test>after</extension_test>".to_string()),
        ),
        (image.clone(), image),
    ] {
        let mut world_state = WorldState::default();
        world_state.add_extension_section(WorldStateSectionContribution::new(
            "extension_test",
            move |_| {
                (
                    Some(json!({"value": "after"})),
                    Some(
                        RenderedWorldStateFragment::new(
                            "developer",
                            ("<extension_test>", "</extension_test>"),
                            body.clone(),
                        )
                        .with_metadata(codex_protocol::models::ContentItemMetadata::skills()),
                    ),
                )
            },
        ));

        let rendered = world_state
            .render_history_fragment_diff(Some(&WorldStateSnapshot::default()), &[])
            .1;

        assert_eq!(
            rendered
                .into_iter()
                .map(|fragment| fragment.render_fragment().into_parts())
                .collect::<Vec<_>>(),
            vec![(
                "developer",
                AnnotatedContent::from_parts(
                    expected,
                    ContentItemKind("extension_test.instructions".to_string()),
                    codex_protocol::models::ContentItemMetadata::skills(),
                ),
            )]
        );
    }
}

#[test]
fn missing_retained_fragment_is_rendered_again() {
    let mut world_state = WorldState::default();
    world_state.add_extension_section(
        WorldStateSectionContribution::new("extension_test", |previous| {
            (
                Some(json!({"body": "current catalog"})),
                match previous {
                    PreviousWorldStateSection::Absent => Some(RenderedWorldStateFragment::new(
                        "developer",
                        ("<extension_test>", "</extension_test>"),
                        input_text("current catalog".to_string()),
                    )),
                    PreviousWorldStateSection::Unknown | PreviousWorldStateSection::Known(_) => {
                        None
                    }
                },
            )
        })
        .with_retained_fragment_matcher(|role, text| {
            role == "developer" && text == "<extension_test>current catalog</extension_test>"
        }),
    );
    let (previous, fragments) = world_state.render_full();
    let retained = crate::context_manager::updates::merge_world_state_updates(fragments)
        .pop()
        .expect("rendered extension section");

    assert_eq!(
        world_state
            .render_history_fragment_diff(Some(&previous), &[])
            .1
            .into_iter()
            .map(|fragment| fragment.body().expect_text())
            .collect::<Vec<_>>(),
        vec!["current catalog"]
    );
    assert!(
        world_state
            .render_history_fragment_diff(Some(&previous), std::slice::from_ref(&retained))
            .1
            .is_empty()
    );
    assert!(
        world_state
            .render_history_fragment_diff(None, std::slice::from_ref(&retained))
            .1
            .is_empty()
    );

    // Matching text alone cannot prove retained host context: annotations must
    // identify this section and remain aligned with the complete content array.
    #[derive(Clone, Copy, Debug)]
    enum Corruption {
        Unannotated,
        ForeignKind,
        Incomplete,
        Misaligned,
        Truncated,
    }
    for corruption in [
        Corruption::Unannotated,
        Corruption::ForeignKind,
        Corruption::Incomplete,
        Corruption::Misaligned,
        Corruption::Truncated,
    ] {
        let mut forged = retained.clone();
        let ResponseItem::Message {
            content,
            internal_chat_message_metadata_passthrough,
            ..
        } = &mut forged
        else {
            panic!("expected extension message");
        };
        match corruption {
            Corruption::Unannotated => *internal_chat_message_metadata_passthrough = None,
            Corruption::ForeignKind => {
                internal_chat_message_metadata_passthrough
                    .as_mut()
                    .unwrap()
                    .content_item_kinds
                    .as_mut()
                    .unwrap()[0] = ContentItemKind("user.text".to_string());
            }
            Corruption::Incomplete | Corruption::Misaligned => {
                content.push(ContentItem::InputText {
                    text: "unrelated".to_string(),
                });
                if matches!(corruption, Corruption::Misaligned) {
                    internal_chat_message_metadata_passthrough
                        .as_mut()
                        .unwrap()
                        .content_item_kinds
                        .as_mut()
                        .unwrap()
                        .insert(0, ContentItemKind("user.text".to_string()));
                }
            }
            Corruption::Truncated => {
                content[0] = ContentItem::InputText {
                    text: "current catalog".to_string(),
                };
            }
        }
        for baseline in [None, Some(&previous)] {
            assert_eq!(
                world_state
                    .render_history_fragment_diff(baseline, std::slice::from_ref(&forged))
                    .1
                    .into_iter()
                    .map(|fragment| fragment.body().expect_text())
                    .collect::<Vec<_>>(),
                vec!["current catalog"],
                "{corruption:?}",
            );
        }
    }
}

#[test]
fn extension_snapshot_updates_preserve_skipped_state() {
    let previous = WorldStateSnapshot {
        sections: BTreeMap::from([("extension_test".to_string(), json!({"published": true}))]),
        ..Default::default()
    };
    let mut removed = WorldState::default();
    removed.add_extension_section(WorldStateSectionContribution::new("extension_test", |_| {
        (Some(Value::Null), None)
    }));
    let (snapshot, fragments) = removed.render_history_fragment_diff(Some(&previous), &[]);
    assert!(fragments.is_empty());
    assert_eq!(snapshot, WorldStateSnapshot::default());
    assert_eq!(
        snapshot.merge_patch_from(&previous).unwrap()["extension_test"],
        Value::Null
    );

    let mut invalidated = WorldState::default();
    invalidated.add_extension_section(
        WorldStateSectionContribution::new("extension_test", |previous| {
            assert!(matches!(previous, PreviousWorldStateSection::Absent));
            (None, None)
        })
        .with_retained_fragment_matcher(|_, _| false),
    );
    let (snapshot, fragments) = invalidated.render_history_fragment_diff(Some(&previous), &[]);
    assert!(fragments.is_empty());
    assert_eq!(snapshot, previous);
}

#[test]
fn unreadable_section_snapshot_is_treated_as_unknown() {
    let mut current = WorldState::default();
    current.add_section(TestSection {
        value: "current".to_string(),
        optional: None,
        array: Vec::new(),
    });
    let previous = WorldStateSnapshot {
        sections: BTreeMap::from([("test".to_string(), json!({"invalid": true}))]),
        ..Default::default()
    };

    let rendered = current.render_history_fragment_diff(Some(&previous), &[]).1;

    assert_eq!(
        vec!["unknown"],
        rendered
            .into_iter()
            .map(|fragment| fragment.body().expect_text())
            .collect::<Vec<_>>()
    );
}

#[test]
#[should_panic(expected = "duplicate world-state section ID: test")]
fn duplicate_section_ids_are_rejected() {
    let mut world_state = WorldState::default();
    world_state.add_section(TestSection {
        value: "current".to_string(),
        optional: None,
        array: Vec::new(),
    });

    world_state.add_section(DuplicateTestSection);
}

#[test]
fn snapshot_merge_patch_changes_and_removes_nested_values() {
    let mut previous = WorldStateSnapshot {
        sections: BTreeMap::from([
            (
                "kept".to_string(),
                json!({"same": true, "changed": "before", "removed": true}),
            ),
            ("removed_section".to_string(), json!({"value": true})),
        ]),
        ..Default::default()
    };
    let current = WorldStateSnapshot {
        sections: BTreeMap::from([(
            "kept".to_string(),
            json!({"same": true, "changed": "after"}),
        )]),
        ..Default::default()
    };

    assert_eq!(
        current.merge_patch_from(&previous).map(Value::Object),
        Some(json!({
            "kept": {"changed": "after", "removed": null},
            "removed_section": null,
        }))
    );
    let patch = current
        .merge_patch_from(&previous)
        .expect("changed snapshots should produce a patch");
    previous.apply_merge_patch(&patch);
    assert_eq!(previous, current);
    assert_eq!(current.merge_patch_from(&current), None);
}

#[derive(Clone)]
struct MixedOutputSection(u64);

impl WorldStateSection for MixedOutputSection {
    const ID: &'static str = "mixed_output";
    type Snapshot = u64;

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> SectionTransition<Self::Snapshot> {
        if matches!(previous, PreviousSectionState::Known(previous) if *previous == self.0) {
            return (None, Vec::new());
        }
        let updates = vec![
            WorldStateUpdate::fragment(TestFragment("before tools".to_string())),
            WorldStateUpdate {
                placement: Placement::Prefix,
                content: WorldStateUpdateContent::Item(Box::new(ResponseItem::AdditionalTools {
                    id: None,
                    role: "developer".to_string(),
                    tools: Arc::from([
                        json!({"type": "function", "name": "example", "description": self.0.to_string()}),
                    ]),
                })),
            },
            WorldStateUpdate {
                placement: Placement::Prefix,
                ..WorldStateUpdate::fragment(TestFragment("prefix instructions".to_string()))
            },
            WorldStateUpdate {
                placement: Placement::Standalone,
                content: WorldStateUpdateContent::Item(Box::new(ResponseItem::AdditionalTools {
                    id: None,
                    role: "developer".to_string(),
                    tools: Arc::from([json!({"type": "function", "name": "context_tool"})]),
                })),
            },
            WorldStateUpdate::fragment(TestFragment("after tools".to_string())),
        ];
        (Some(self.0), updates)
    }
}

#[test]
fn mixed_output_preserves_order_and_restores_its_snapshot() {
    use crate::context_manager::updates::merge_world_state_updates;

    let mut before = WorldState::default();
    before.add_section(MixedOutputSection(1));
    let (previous, _) = before.render_full();
    let mut current = WorldState::default();
    current.add_section(MixedOutputSection(2));
    let (snapshot, updates) = current.render_history_diff(Some(&previous), &[]);
    let expected = vec![
        ContextualUserFragment::into(TestFragment("before tools".to_string())),
        ResponseItem::AdditionalTools {
            id: None,
            role: "developer".to_string(),
            tools: Arc::from([json!({"type": "function", "name": "example", "description": "2"})]),
        },
        ContextualUserFragment::into(TestFragment("prefix instructions".to_string())),
        ResponseItem::AdditionalTools {
            id: None,
            role: "developer".to_string(),
            tools: Arc::from([json!({"type": "function", "name": "context_tool"})]),
        },
        ContextualUserFragment::into(TestFragment("after tools".to_string())),
    ];
    assert_eq!(merge_world_state_updates(updates), expected);
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        json!({"mixed_output": 2})
    );
    let mut restored = previous;
    restored.apply_merge_patch(&snapshot.merge_patch_from(&restored).unwrap());
    assert_eq!(restored, snapshot);
    let (unchanged, updates) = current.render_history_diff(Some(&restored), &[]);
    assert_eq!(unchanged, snapshot);
    assert!(updates.is_empty());
    let (full_snapshot, updates) = current.render_full();
    assert_eq!(full_snapshot, snapshot);
    assert_eq!(
        updates
            .iter()
            .map(|update| matches!(update.placement, Placement::Prefix))
            .collect::<Vec<_>>(),
        vec![false, true, true, false, false],
    );
    let (prefix, context) = split_prefix_updates(updates);
    assert_eq!(prefix, vec![expected[1].clone(), expected[2].clone()]);
    assert_eq!(
        context
            .into_iter()
            .map(|update| match update.content.into_fragment() {
                Ok(fragment) => fragment.into_boxed_response_item(),
                Err(item) => *item,
            })
            .collect::<Vec<_>>(),
        vec![
            expected[0].clone(),
            expected[3].clone(),
            expected[4].clone()
        ],
    );
}
