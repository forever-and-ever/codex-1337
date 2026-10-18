use std::sync::Arc;

/// A live reason automatic idle work must not start. Ordinary input is unaffected.
#[derive(Clone)]
pub struct IdleHold(Arc<dyn Fn() -> bool + Send + Sync>);

impl IdleHold {
    pub fn new(held: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(held))
    }

    #[must_use]
    pub fn is_held(&self) -> bool {
        (self.0)()
    }
}

impl std::fmt::Debug for IdleHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdleHold").finish_non_exhaustive()
    }
}
