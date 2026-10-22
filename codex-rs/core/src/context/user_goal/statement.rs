//! Whether a recorded user goal instruction states its objective.

use super::UserGoalUpdate;
use crate::context::ContextualUserFragment;

impl UserGoalUpdate {
    /// True when this instruction states the whole objective; status-only updates and
    /// objectives over the evidence limit state none.
    pub fn states_objective(&self) -> bool {
        matches!(
            self,
            Self::Set {
                objective: Some(_),
                ..
            }
        ) && self.content_kind().0 != Self::OMITTED_OBJECTIVE_KIND
    }
}
