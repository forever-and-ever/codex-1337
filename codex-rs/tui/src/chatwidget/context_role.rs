use super::ChatWidget;
use crate::app_event::AppEvent;
use codex_app_server_protocol::ContextRole;

#[derive(Debug)]
pub(crate) enum ContextRoleCommand {
    Show,
    Set(Option<ContextRole>),
}

impl ChatWidget {
    pub(super) fn dispatch_context_role(&mut self, argument: &str) {
        let command = match argument.trim() {
            "" => ContextRoleCommand::Show,
            "clear" => ContextRoleCommand::Set(None),
            text => match ContextRole::new(text) {
                Ok(role) => ContextRoleCommand::Set(Some(role)),
                Err(err) => {
                    self.add_error_message(err.to_owned());
                    return;
                }
            },
        };
        if let Some(thread_id) = self.thread_id {
            self.app_event_tx
                .send(AppEvent::ContextRole { thread_id, command });
        } else {
            self.add_error_message(
                "Session is still starting; try /contextrole again shortly.".to_string(),
            );
        }
    }

    pub(crate) fn show_context_role(&mut self, role: Option<&str>) {
        let message = match role {
            Some(role) => format!("Context role: {role}."),
            None => "No context role.".to_owned(),
        };
        self.add_info_message(message, /*hint*/ None);
    }

    pub(crate) fn context_role_cleared(&mut self) {
        self.add_info_message("Context role cleared.".to_owned(), /*hint*/ None);
    }
}
