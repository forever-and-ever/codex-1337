//! Canonical base instructions for ordinary windows; live changes are append-only amendments.

use super::PreviousSectionState;
use super::SectionTransition;
use super::WorldStateHash;
use super::WorldStateSection;
use super::WorldStateUpdate;
use crate::context::BaseInstructionsFragment;
use crate::context::ContextualUserFragment;

pub(crate) struct BaseInstructionsState(pub(crate) String);

impl WorldStateSection for BaseInstructionsState {
    const ID: &'static str = "base_instructions";
    type Snapshot = WorldStateHash;

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> SectionTransition<Self::Snapshot> {
        let fragment = BaseInstructionsFragment(self.0.clone());
        let hash = WorldStateHash::from_fragment(&fragment);
        let updates = if matches!(previous, PreviousSectionState::Absent) && !self.0.is_empty() {
            vec![WorldStateUpdate::prefix_item(ContextualUserFragment::into(
                fragment,
            ))]
        } else {
            Vec::new()
        };
        (Some(hash), updates)
    }
}
