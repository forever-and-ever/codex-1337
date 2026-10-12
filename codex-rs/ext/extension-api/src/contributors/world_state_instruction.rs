use super::RenderedWorldStateFragment;
use super::WorldStateSectionContribution;

#[derive(Clone)]
pub enum WorldStateSectionMode {
    Context,
    /// The complete current instructions, supplied without render side effects.
    Instruction(Option<RenderedWorldStateFragment>),
}

impl WorldStateSectionContribution {
    /// Ordinary renders provide semantic amendments. The harness owns retained
    /// revision proofs and uses `current` to resolve amendments before compaction.
    pub fn with_instruction_amendments(
        mut self,
        current: Option<RenderedWorldStateFragment>,
    ) -> Self {
        self.mode = WorldStateSectionMode::Instruction(current);
        self
    }

    pub fn mode(&self) -> &WorldStateSectionMode {
        &self.mode
    }
}
