use super::WorldStateSectionContribution;

impl WorldStateSectionContribution {
    /// The absent-state render is this section's complete current instruction value.
    /// Ordinary renders may provide semantic amendments. The harness owns retained
    /// revision proofs and resolves those amendments before compaction.
    pub fn with_instruction_amendments(mut self) -> Self {
        self.instruction_amendments = true;
        self
    }

    pub fn uses_instruction_amendments(&self) -> bool {
        self.instruction_amendments
    }
}
