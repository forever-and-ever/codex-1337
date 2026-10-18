use super::DIFF_KIND_PREFIX;
use super::Revision;
use super::STATE_PREFIX;
use super::WorldStateSnapshot;
use super::fingerprint;
use crate::context::ContextualUserFragment;
use codex_context_fragments::AnnotatedContent;
use codex_context_fragments::set_annotated_content;
use codex_context_fragments::to_annotated_content;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

impl WorldStateSnapshot {
    /// Rewrites only a disposable compaction input. Persisted history is never edited.
    pub(crate) fn materialize_instructions(
        &self,
        current: &Self,
        items: Vec<ResponseItemEnvelope>,
    ) -> Vec<ResponseItemEnvelope> {
        let fresh = revisions(current);
        let mut revisions = revisions(self);
        let mut owners = BTreeMap::new();
        let mut kinds = BTreeMap::new();
        for (id, revision) in revisions.iter().chain(&fresh) {
            if let Some(text) = &revision.current {
                let key = (text.role().to_string(), text.kind.0.clone());
                kinds
                    .entry(key)
                    .and_modify(|owner: &mut Option<String>| {
                        if owner.as_ref() != Some(id) {
                            *owner = None;
                        }
                    })
                    .or_insert_with(|| Some(id.clone()));
                owners.insert(text.fingerprint().0, id.clone());
            }
        }
        for (id, revision) in &revisions {
            for hash in &revision.lineage {
                owners.insert(hash.0.clone(), id.clone());
            }
        }
        for (id, revision) in &mut revisions {
            revision.current = fresh.get(id).and_then(|current| current.current.clone());
        }
        // A newly configured section has no historical amendment lineage yet.
        for (id, revision) in fresh {
            revisions
                .entry(id)
                .and_modify(|old| old.current = revision.current.clone())
                .or_insert(revision);
        }
        let mut emitted = BTreeSet::new();
        let mut result = Vec::with_capacity(items.len());
        for mut envelope in items {
            let ResponseItem::Message {
                role,
                content,
                internal_chat_message_metadata_passthrough: Some(metadata),
                ..
            } = &envelope.item
            else {
                result.push(envelope);
                continue;
            };
            // Partial positional annotations are not proof of instruction ownership.
            if content.is_empty()
                || metadata
                    .content_item_kinds
                    .as_ref().is_none_or(|kinds| kinds.len() != content.len())
            {
                result.push(envelope);
                continue;
            }
            let role = role.clone();
            let Some(content) = to_annotated_content(&mut envelope.item) else {
                unreachable!()
            };
            let mut changed = false;
            let mut materialized = Vec::with_capacity(content.len());
            for part in content {
                if let Some(id) = part.kind().as_str().strip_prefix(DIFF_KIND_PREFIX) {
                    // A full reinjection may reset the ledger while earlier amendments
                    // survive. Their host-owned section identity still proves ownership.
                    if !emitted.contains(id)
                        && let Some(Some(text)) =
                            revisions.get(id).map(|revision| revision.current.as_ref())
                    {
                        assert_eq!(role, text.role(), "instruction amendment changed authority");
                        materialized
                            .push(AnnotatedContent::input_text(&text.text, text.kind.clone()));
                        emitted.insert(id.to_string());
                    }
                    changed = true;
                    continue;
                }
                let owner = match part.content() {
                    ContentItem::InputText { text } => {
                        owners
                            .get(&fingerprint(&role, part.kind(), text).0)
                            .or_else(|| {
                                // Older windows lack a revision ledger. Only host-annotated,
                                // unambiguous instruction kinds can be migrated by identity.
                                kinds
                                    .get(&(role.clone(), part.kind().0.clone()))
                                    .and_then(Option::as_ref)
                            })
                    }
                    _ => None,
                };
                let Some(owner) = owner else {
                    materialized.push(part);
                    continue;
                };
                changed = true;
                if emitted.insert(owner.clone())
                    && let Some(text) = &revisions[owner].current
                {
                    assert_eq!(
                        role,
                        text.role(),
                        "materialized instruction changed authority"
                    );
                    materialized.push(AnnotatedContent::input_text(&text.text, text.kind.clone()));
                }
            }
            if materialized.is_empty() {
                continue;
            }
            assert!(
                set_annotated_content(&mut envelope.item, materialized).is_some(),
                "message remains a message"
            );
            if changed {
                // The rewritten text is not the original provider item or an original user receipt.
                envelope.item.set_id(None);
                if let Some(metadata) = &mut envelope.metadata {
                    metadata.mark_retained_sources_incomplete();
                }
            }
            result.push(envelope);
        }
        let additions = revisions
            .into_iter()
            .filter_map(|(id, revision)| {
                (!emitted.contains(&id))
                    .then_some(revision.current)
                    .flatten()
                    .map(|text| ResponseItemEnvelope::new(ContextualUserFragment::into(text)))
            })
            .collect::<Vec<_>>();
        // A signed checkpoint must retain its provider-mandated leading position.
        let insertion = result
            .iter()
            .take_while(|item| {
                matches!(
                    item.item,
                    ResponseItem::NativeCompaction { .. } | ResponseItem::Compaction { .. }
                )
            })
            .count();
        result.splice(insertion..insertion, additions);
        result
    }
}

fn revisions(snapshot: &WorldStateSnapshot) -> BTreeMap<String, Revision> {
    snapshot.sections.iter().filter_map(|(key, value)| {
        let id = key.strip_prefix(STATE_PREFIX)?;
        match serde_json::from_value(value.clone()) {
            Ok(revision) => Some((id.to_string(), revision)),
            Err(error) => {
                tracing::warn!(section = id, %error, "unreadable instruction revision; preserving compaction source");
                None
            }
        }
    }).collect()
}
