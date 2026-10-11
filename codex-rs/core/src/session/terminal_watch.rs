use super::session::Session;

impl Session {
    pub(crate) async fn terminal_runtime_closed(&self) -> bool {
        self.state.lock().await.shutting_down
    }
}
