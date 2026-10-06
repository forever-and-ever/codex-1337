use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct NetworkRequestDisconnect;

impl NetworkRequestDisconnect {
    pub fn elapsed(&self) -> Option<Duration> {
        None
    }
}
