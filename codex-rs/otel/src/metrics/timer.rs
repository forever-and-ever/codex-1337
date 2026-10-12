use super::Result;

/// Timers cannot exist without an active metrics client.
#[derive(Debug)]
pub enum Timer {}

// Preserve the upstream RAII contract at call sites, even though no timer can exist.
impl Drop for Timer {
    fn drop(&mut self) {
        match *self {}
    }
}

impl Timer {
    pub fn record(&self, _additional_tags: &[(&str, &str)]) -> Result<()> {
        match *self {}
    }
}
