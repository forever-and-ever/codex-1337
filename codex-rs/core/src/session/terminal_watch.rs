use super::session::Session;

impl Session {
    pub(crate) fn terminal_runtime_closed(&self) -> bool {
        self.runtime_holds.is_closing()
    }
}
