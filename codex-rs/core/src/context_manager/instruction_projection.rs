use super::ContextManager;
use crate::context::world_state::WorldStateSnapshot;

impl ContextManager {
    pub(crate) fn for_compaction(mut self, current: &WorldStateSnapshot) -> Self {
        let mut items = self
            .world_state_baseline
            .as_ref()
            .unwrap_or(&WorldStateSnapshot::default())
            .materialize_instructions(current, self.annotated_items().to_vec());
        // TELOS(instruction-amendments): rewritten compaction input has fresh request identities,
        // not the identities of the historical instructions it replaces.
        for envelope in &mut items {
            crate::session::assign_missing_item_id(&mut envelope.item, &self.item_id_generator);
        }
        self.items = std::sync::Arc::new(items);
        self
    }

    pub(crate) fn take_base_instructions(&mut self) -> Option<String> {
        crate::context::BaseInstructionsFragment::take_annotated_prefix(std::sync::Arc::make_mut(
            &mut self.items,
        ))
    }
}
