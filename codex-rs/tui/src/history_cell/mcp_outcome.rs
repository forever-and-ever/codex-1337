//! One invocation outcome vocabulary across compact, expanded and raw projections.

use super::McpToolCallCell;

impl McpToolCallCell {
    pub(super) fn outcome_verb(&self) -> &'static str {
        match self.success() {
            Some(true) => "Called",
            Some(false) => "Failed",
            None => "Calling",
        }
    }
}
