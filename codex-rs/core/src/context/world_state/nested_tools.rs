//! The nested-tool roster is context a window states once and a catalogue change amends, so
//! the `exec` declaration never moves. It is not a managed instruction: compaction reads the
//! roster and its amendments as the window holds them, and its request keeps the cached prefix.

use super::PermissionsState;
use super::PreviousSectionState;
use super::SectionTransition;
use super::WorldState;
use super::WorldStateContextFragment;
use super::WorldStateSection;
use super::WorldStateUpdate;
use super::instruction_diff::Entry;
use crate::context::ContextualUserFragment;
use codex_code_mode::ROSTER_TAGS;
use codex_code_mode::RosterEntry;
use codex_code_mode::RosterRecord;
use codex_extension_api::RenderedWorldStateFragment;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use codex_protocol::models::input_text;
use std::sync::Arc;

/// `None` lets the roster the thread inherited stand, saying nothing.
struct NestedToolsState(Option<Arc<[RosterEntry]>>);

impl WorldStateSection for NestedToolsState {
    const ID: &'static str = "nested_tools";
    type Snapshot = RosterRecord;

    fn should_persist(&self) -> bool {
        self.0.as_deref().is_some_and(|entries| !entries.is_empty())
    }

    fn has_retained_fragment_matcher() -> bool {
        true
    }

    fn matches_retained_fragment(role: &str, text: &str) -> bool {
        role == "developer" && text.starts_with(ROSTER_TAGS.0)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, RosterRecord>,
    ) -> SectionTransition<RosterRecord> {
        let Some(entries) = self.0.as_deref() else {
            return (None, Vec::new());
        };
        let current = codex_code_mode::roster_record(entries);
        let body = match previous {
            PreviousSectionState::Known(told) => {
                codex_code_mode::render_roster_amendment(&current, told)
            }
            PreviousSectionState::Absent | PreviousSectionState::Unknown => {
                codex_code_mode::render_roster(entries)
            }
        };
        let update = body.map(|body| {
            WorldStateUpdate::fragment(
                WorldStateContextFragment {
                    fragment: RenderedWorldStateFragment::new(
                        "developer",
                        ROSTER_TAGS,
                        input_text(body),
                    ),
                    content_kind: ContentItemKind("tools.nested_roster".to_string()),
                }
                .with_metadata(ContentItemMetadata::tool(/*namespace*/ None)),
            )
        });
        (Some(current), update.into_iter().collect())
    }
}

impl WorldState {
    /// Lists the nested tools beside the other catalogues, ahead of the standing rules.
    /// Without a current roster, the one the thread inherited stands unamended.
    pub(crate) fn add_nested_tools(&mut self, nested_tools: Option<Arc<[RosterEntry]>>) {
        let id = NestedToolsState::ID;
        assert!(
            !self.sections.contains_key(id),
            "duplicate world-state section ID: {id}"
        );
        let entry = Entry::Context(Box::new(NestedToolsState(nested_tools)));
        match self.sections.get_index_of(PermissionsState::ID) {
            Some(index) => self.sections.shift_insert(index, id, entry),
            None => self.sections.insert(id, entry),
        };
    }
}
