//! Model identity is context, independent of canonical base-instruction ownership.

use super::PreviousSectionState;
use super::SectionTransition;
use super::WorldStateSection;
use super::WorldStateSnapshot;
use super::WorldStateUpdate;
use crate::context::ModelInstructions;
use serde_json::Value;

pub(crate) struct ModelIdentityState(pub(crate) String);

impl WorldStateSection for ModelIdentityState {
    const ID: &'static str = "model_identity";
    type Snapshot = String;

    fn should_persist(&self) -> bool {
        // The upstream model snapshot is the sole comparison-state owner.
        false
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> SectionTransition<Self::Snapshot> {
        let changed = matches!(previous, PreviousSectionState::Known(model) if model != &self.0);
        let updates = WorldStateUpdate::optional_standalone_boxed_fragment(changed.then(|| {
            Box::new(ModelInstructions {
                model: Some(self.0.clone()),
                instructions: None,
            }) as _
        }));
        (Some(self.0.clone()), updates)
    }
}

/// Migrate identity from the exact upstream comparison snapshot, not from message text.
/// Full renders do not consult this path and never add a redundant identity announcement.
pub(super) fn previous_snapshot<'a>(
    snapshot: &'a WorldStateSnapshot,
    id: &str,
) -> Option<&'a Value> {
    snapshot.sections.get(id).or_else(|| {
        if id != ModelIdentityState::ID {
            return None;
        }
        match snapshot.sections.get("model")? {
            model @ Value::String(_) => Some(model),
            Value::Object(model) => model.get("model").filter(|model| model.is_string()),
            _ => None,
        }
    })
}
