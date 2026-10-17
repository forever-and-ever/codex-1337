//! A live UI enables preparation once; every widget shares its requester.

use super::FrameRequester;
use std::sync::atomic::Ordering;

impl FrameRequester {
    pub(crate) fn enable_background_preparation(&self) {
        self.background_preparation.store(true, Ordering::Relaxed);
    }

    pub(crate) fn prepares_in_background(&self) -> bool {
        self.background_preparation.load(Ordering::Relaxed)
    }
}
