//! Full instruction values belong to the harness; amendments alone enter a live window.
//! Every section renders through here, so a blank fragment with nothing to revoke never enters.

#[path = "instruction_projection.rs"]
mod projection;

use super::ErasedWorldStateSection;
use super::PreviousSectionState;
use super::SectionTransition;
use super::WorldState;
use super::WorldStateContextFragment;
use super::WorldStateHash;
use super::WorldStateSection;
use super::WorldStateSnapshot;
use super::WorldStateUpdate;
use super::WorldStateUpdateContent;
use super::hash_component;
use crate::context::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use sha1::Digest;
use sha1::Sha1;
use similar::TextDiff;

const STATE_PREFIX: &str = "codex.instruction/";
const DIFF_KIND_PREFIX: &str = "context.instruction_diff/";
const OPEN: &str = "<instruction_diff>";
const CLOSE: &str = "</instruction_diff>";

pub(super) enum Entry {
    Context(Box<dyn ErasedWorldStateSection>),
    Instruction(Box<dyn ErasedWorldStateSection>, Amendment),
}

pub(super) enum Amendment {
    Unified(&'static str),
    Semantic(Option<WorldStateContextFragment>),
}

impl Entry {
    pub(super) fn renderer(&self) -> &dyn ErasedWorldStateSection {
        match self {
            Self::Context(section) | Self::Instruction(section, _) => section.as_ref(),
        }
    }

    pub(super) fn render(
        &self,
        id: &str,
        previous: PreviousSectionState<'_, serde_json::Value>,
        stored: Option<&WorldStateSnapshot>,
        retained: &[WorldStateHash],
        snapshot: &mut WorldStateSnapshot,
    ) -> SectionTransition {
        let Self::Instruction(section, amendment) = self else {
            return render_eloquent(self.renderer(), previous);
        };
        let instruction_id = match amendment {
            Amendment::Unified(identity) => *identity,
            Amendment::Semantic(_) => id,
        };
        let prior = stored
            .and_then(|snapshot| revision(snapshot, instruction_id))
            .filter(|revision| revision.retained(retained));
        let (current, state, rendered) = match amendment {
            Amendment::Unified(_) => {
                let (state, full) = render_eloquent(section.as_ref(), PreviousSectionState::Absent);
                let Canonical::Instruction(current) = canonical(&full) else {
                    // Native declarations are not replaceable instruction text.
                    return render_eloquent(section.as_ref(), previous);
                };
                if prior.is_none()
                    && current.is_none()
                    && !matches!(previous, PreviousSectionState::Absent)
                {
                    // Legacy history has no canonical predecessor to revoke by diff.
                    // Keep its explicit revocation outside materialization until a full
                    // instruction value establishes ownership of the older fragments.
                    return render_eloquent(section.as_ref(), previous);
                }
                (current, state, full)
            }
            Amendment::Semantic(current) => {
                let previous = if prior.is_some() {
                    previous
                } else {
                    PreviousSectionState::Absent
                };
                let (state, updates) = render_eloquent(section.as_ref(), previous);
                (
                    current
                        .as_ref()
                        .filter(|fragment| !vacuous(*fragment))
                        .map(|fragment| Text::from_fragment(fragment)),
                    state,
                    updates,
                )
            }
        };
        let (mut revision, updates) = match prior {
            // Kind and provenance are host bookkeeping the model never reads: a revision that
            // differs only there is adopted silently instead of amended by an empty patch.
            Some(mut prior) if reading(prior.current.as_ref()) == reading(current.as_ref()) => {
                prior.current = current;
                (prior, Vec::new())
            }
            Some(mut prior) => {
                let updates = match amendment {
                    Amendment::Unified(_) => {
                        diff(instruction_id, prior.current.as_ref(), current.as_ref())
                            .map(WorldStateUpdate::fragment)
                            .into_iter()
                            .collect()
                    }
                    Amendment::Semantic(_) => rendered,
                };
                // A semantic renderer may intentionally suppress a budget-only change.
                if !updates.is_empty() {
                    prior.current = current;
                }
                (prior, updates)
            }
            None => {
                let mut revision = Revision {
                    current,
                    lineage: Vec::new(),
                };
                let already_present = revision.current.as_ref().is_some_and(|text| {
                    let hash = text.fingerprint();
                    if retained.contains(&hash) {
                        revision.lineage.push(hash);
                        true
                    } else {
                        false
                    }
                });
                let updates = if already_present {
                    Vec::new()
                } else {
                    rendered
                };
                (revision, updates)
            }
        };
        for update in &updates {
            if let Canonical::Instruction(Some(text)) = canonical(std::slice::from_ref(update)) {
                revision.lineage.push(text.fingerprint());
            }
        }
        snapshot.sections.insert(
            format!("{STATE_PREFIX}{instruction_id}"),
            revision.into_value(),
        );
        (state, updates)
    }
}

impl WorldState {
    pub(crate) fn add_instruction_section<S: WorldStateSection>(&mut self, section: S) {
        self.add_instruction_section_with_identity(section, S::ID);
    }

    /// Keeps canonical instruction ownership stable when upstream comparison sections migrate.
    pub(crate) fn add_instruction_section_with_identity<S: WorldStateSection>(
        &mut self,
        section: S,
        identity: &'static str,
    ) {
        let id = S::ID;
        assert!(
            !self.sections.contains_key(id),
            "duplicate world-state section ID: {id}"
        );
        self.sections.insert(
            id,
            Entry::Instruction(Box::new(section), Amendment::Unified(identity)),
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Authority {
    User,
    Developer,
    System,
}

impl Authority {
    fn parse(role: &str) -> Self {
        match role {
            "user" => Self::User,
            "developer" => Self::Developer,
            "system" => Self::System,
            _ => unreachable!(
                "instruction sections only have user, developer, or system authority, got {role}"
            ),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Developer => "developer",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Text {
    authority: Authority,
    kind: ContentItemKind,
    text: String,
    #[serde(default)]
    metadata: codex_protocol::models::ContentItemMetadata,
}

impl Text {
    fn from_fragment(fragment: &dyn ContextualUserFragment) -> Self {
        Self {
            authority: Authority::parse(fragment.role()),
            kind: fragment.content_kind(),
            text: match fragment.render() {
                ContentItem::InputText { text } => text,
                _ => unreachable!("instruction amendments require text"),
            },
            metadata: fragment.content_metadata(),
        }
    }

    fn fingerprint(&self) -> WorldStateHash {
        fingerprint(self.authority.as_str(), &self.kind, &self.text)
    }
}

impl ContextualUserFragment for Text {
    fn role(&self) -> &'static str {
        self.authority.as_str()
    }
    fn content_metadata(&self) -> codex_protocol::models::ContentItemMetadata {
        self.metadata.clone()
    }
    fn content_kind(&self) -> ContentItemKind {
        self.kind.clone()
    }
    fn markers(&self) -> (&'static str, &'static str) {
        ("", "")
    }
    fn body(&self) -> ContentItem {
        codex_protocol::models::input_text(self.text.clone())
    }
    fn type_markers() -> (&'static str, &'static str) {
        (OPEN, CLOSE)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Revision {
    current: Option<Text>,
    lineage: Vec<WorldStateHash>,
}

impl Revision {
    fn retained(&self, retained: &[WorldStateHash]) -> bool {
        let mut remaining = self.lineage.iter();
        let mut next = remaining.next();
        for hash in retained {
            if next == Some(hash) {
                next = remaining.next();
            }
        }
        next.is_none()
    }

    #[expect(
        clippy::expect_used,
        reason = "revision fields are strings, enums and collections with infallible JSON representations"
    )]
    fn into_value(self) -> serde_json::Value {
        serde_json::to_value(self).expect("instruction revision is serializable")
    }
}

fn revision(snapshot: &WorldStateSnapshot, id: &str) -> Option<Revision> {
    snapshot.sections.get(&format!("{STATE_PREFIX}{id}")).and_then(|value| {
        serde_json::from_value(value.clone()).map_err(|error| {
            tracing::warn!(section = id, %error, "unreadable instruction revision; reannouncing full text");
        }).ok()
    })
}

pub(super) fn has_revision(
    snapshot: &WorldStateSnapshot,
    id: &str,
    retained: &[WorldStateHash],
) -> Option<bool> {
    revision(snapshot, id).map(|revision| revision.retained(retained))
}

pub(super) fn retire_missing(
    stored: Option<&WorldStateSnapshot>,
    snapshot: &mut WorldStateSnapshot,
    retained: &[WorldStateHash],
) -> Vec<WorldStateUpdate> {
    let mut updates = Vec::new();
    let Some(stored) = stored else { return updates };
    for (key, id) in stored
        .sections
        .keys()
        .filter_map(|key| key.strip_prefix(STATE_PREFIX).map(|id| (key, id)))
    {
        if snapshot.sections.contains_key(key) {
            continue;
        }
        let Some(mut prior) = revision(stored, id) else {
            continue;
        };
        let was_retained = prior.retained(retained);
        if let Some(before) = prior.current.take()
            && was_retained
            && let Some(removal) = diff(id, Some(&before), None)
        {
            prior.lineage.push(removal.fingerprint());
            updates.push(WorldStateUpdate::fragment(removal));
        }
        snapshot.sections.insert(key.clone(), prior.into_value());
    }
    updates
}

enum Canonical {
    Opaque,
    Instruction(Option<Text>),
}

fn canonical(updates: &[WorldStateUpdate]) -> Canonical {
    match updates {
        [] => Canonical::Instruction(None),
        [
            WorldStateUpdate {
                content: WorldStateUpdateContent::Fragment(fragment),
                ..
            },
        ] => Canonical::Instruction(Some(Text::from_fragment(fragment.as_ref()))),
        [
            WorldStateUpdate {
                content: WorldStateUpdateContent::Item(item),
                ..
            },
        ] if crate::context::BaseInstructionsFragment::text_from_item(item).is_some() => {
            let text = crate::context::BaseInstructionsFragment::text_from_item(item)
                .unwrap_or_else(|| unreachable!("guard verified dedicated base item"));
            Canonical::Instruction(Some(Text::from_fragment(
                &crate::context::BaseInstructionsFragment(text.to_owned()),
            )))
        }
        _ => Canonical::Opaque,
    }
}

/// Renders a section. A blank fragment can only revoke an earlier one, so with nothing before it
/// the fragment says nothing and is dropped; a blank full instruction is no instruction at all.
fn render_eloquent(
    section: &dyn ErasedWorldStateSection,
    previous: PreviousSectionState<'_, serde_json::Value>,
) -> SectionTransition {
    let first = matches!(previous, PreviousSectionState::Absent);
    let (state, mut updates) = section.render_diff(previous);
    if first {
        updates.retain(|update| match &update.content {
            WorldStateUpdateContent::Fragment(fragment) => !vacuous(fragment.as_ref()),
            WorldStateUpdateContent::Item(_) | WorldStateUpdateContent::Environment(_) => true,
        });
    }
    (state, updates)
}

fn vacuous(fragment: &dyn ContextualUserFragment) -> bool {
    matches!(fragment.body(), ContentItem::InputText { text } if text.trim().is_empty())
}

/// The model reads an instruction's authority and text, nothing more.
fn reading(text: Option<&Text>) -> Option<(&Authority, &str)> {
    text.map(|text| (&text.authority, text.text.as_str()))
}

/// Patches `before` into `after`; agreeing texts need no amendment.
fn diff(id: &str, before: Option<&Text>, after: Option<&Text>) -> Option<Text> {
    if let (Some(before), Some(after)) = (before, after) {
        assert_eq!(
            before.authority, after.authority,
            "instruction section {id} changed authority"
        );
    }
    let old = before.map_or("", |text| text.text.as_str());
    let new = after.map_or("", |text| text.text.as_str());
    if old == new {
        return None;
    }
    let Some(endpoint) = after.or(before) else {
        unreachable!("instruction change has no endpoint: {id}");
    };
    let authority = endpoint.authority.clone();
    let from = before.map_or_else(|| "absent".to_string(), |text| text.fingerprint().0);
    let to = after.map_or_else(|| "removed".to_string(), |text| text.fingerprint().0);
    let patch = TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("{id}/{from}"), &format!("{id}/{to}"))
        .to_string();
    Some(Text {
        authority,
        metadata: endpoint.metadata.clone(),
        kind: ContentItemKind(format!("{DIFF_KIND_PREFIX}{id}")),
        text: format!(
            "{OPEN}\nApply this update to the previously supplied {id} instructions; unchanged instructions remain in force.\n{patch}{CLOSE}"
        ),
    })
}

fn fingerprint(role: &str, kind: &ContentItemKind, text: &str) -> WorldStateHash {
    let mut hasher = Sha1::new();
    hasher.update(b"codex-instruction-v1\0");
    hash_component(&mut hasher, role);
    hash_component(&mut hasher, &kind.0);
    hash_component(&mut hasher, text);
    WorldStateHash(format!("{:x}", hasher.finalize()))
}

pub(super) fn retained<'a>(
    items: impl IntoIterator<Item = &'a ResponseItem>,
) -> Vec<WorldStateHash> {
    let mut hashes = Vec::new();
    for item in items {
        let ResponseItem::Message {
            role,
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = item
        else {
            continue;
        };
        let Some(kinds) = metadata
            .content_item_kinds
            .as_ref()
            .filter(|kinds| kinds.len() == content.len())
        else {
            continue;
        };
        for (content, kind) in content.iter().zip(kinds) {
            if let ContentItem::InputText { text } = content {
                hashes.push(fingerprint(role, kind, text));
            }
        }
    }
    hashes
}

pub(crate) fn is_instruction_diff(text: &str) -> bool {
    Text::matches_text(text)
}
