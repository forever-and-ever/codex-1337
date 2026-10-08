use super::ChatWidget;
use crate::app_event::AppEvent;
use codex_app_server_protocol::CommsGate;

impl ChatWidget {
    pub(super) fn dispatch_comms(&mut self, argument: &str) {
        let comms = match argument.trim() {
            "" => None,
            "open" => Some(CommsGate::Open),
            "close" => Some(CommsGate::Closed),
            "block" => Some(CommsGate::Blocked),
            _ => {
                self.add_error_message("Usage: /comms [open|close|block]".to_string());
                return;
            }
        };
        if let Some(thread_id) = self.thread_id {
            self.app_event_tx.send(AppEvent::Comms { thread_id, comms });
        } else {
            self.add_error_message(
                "Session is still starting; try /comms again shortly.".to_string(),
            );
        }
    }

    pub(crate) fn show_comms(&mut self, comms: CommsGate) {
        self.add_info_message(format!("Comms: {}.", comms.as_str()), /*hint*/ None);
    }
}
