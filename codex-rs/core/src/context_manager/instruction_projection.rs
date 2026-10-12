use super::ContextManager;
use crate::context::world_state::WorldStateSnapshot;

impl ContextManager {
    pub(crate) fn for_compaction(mut self, current: &WorldStateSnapshot) -> Self {
        self.items = std::sync::Arc::new(
            self.world_state_baseline
                .as_ref()
                .unwrap_or(&WorldStateSnapshot::default())
                .materialize_instructions(current, self.annotated_items().to_vec()),
        );
        self
    }

    pub(crate) fn take_base_instructions(&mut self) -> Option<String> {
        crate::context::BaseInstructionsFragment::take_annotated_prefix(std::sync::Arc::make_mut(
            &mut self.items,
        ))
    }
}
