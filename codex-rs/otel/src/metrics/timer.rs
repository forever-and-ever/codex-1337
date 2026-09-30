use super::Result;

/// Timers cannot exist without an active metrics client.
#[derive(Debug)]
pub enum Timer {}

impl Timer {
    pub fn record(&self, _additional_tags: &[(&str, &str)]) -> Result<()> {
        match *self {}
    }
}
