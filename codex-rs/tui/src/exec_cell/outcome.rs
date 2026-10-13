//! Command presentation distinguishes rejected work from an executed process.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandOutcome {
    Exited(i32),
    Declined,
}

impl Default for CommandOutcome {
    fn default() -> Self {
        Self::Exited(0)
    }
}

impl From<i32> for CommandOutcome {
    fn from(exit_code: i32) -> Self {
        Self::Exited(exit_code)
    }
}

impl CommandOutcome {
    pub(crate) fn is_success(self) -> bool {
        self == Self::Exited(0)
    }
}
