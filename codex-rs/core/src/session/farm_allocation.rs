//! Allocation certainty is monotone, including failed acquisition and cleanup.

use codex_protocol::ThreadId;
use std::sync::Mutex;
use std::sync::PoisonError;

#[derive(Clone, Copy, Default)]
pub(crate) enum FarmAllocationPhase {
    #[default]
    BeforeCreation,
    CreationMayHaveAllocated(ThreadId),
    Allocated(ThreadId),
}

#[derive(Default)]
pub(crate) struct FarmAllocation {
    phase: Mutex<FarmAllocationPhase>,
}

impl FarmAllocation {
    pub(crate) fn phase(&self) -> FarmAllocationPhase {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn begin_creation(&self, child: ThreadId) {
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(matches!(*phase, FarmAllocationPhase::BeforeCreation));
        *phase = FarmAllocationPhase::CreationMayHaveAllocated(child);
    }

    pub(crate) fn allocated(&self, child: ThreadId) {
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(matches!(*phase, FarmAllocationPhase::CreationMayHaveAllocated(id) if id == child));
        *phase = FarmAllocationPhase::Allocated(child);
    }
}
