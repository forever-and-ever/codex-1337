use super::ChatWidget;
use crate::app_event::AppEvent;
use codex_app_server_protocol::ContextRole;

#[derive(Debug)]
pub(crate) enum ContextRoleCommand {
    Show,
    Set(Option<ContextRole>),
}

const CONTEXT_ROLE_USAGE: &str = "Usage: /contextrole [set <role>|clear]";
pub(crate) const CONTEXT_ROLE_DESCRIPTION: &str =
    "show the context role; /contextrole set <role> changes it";

impl ContextRoleCommand {
    fn parse(argument: &str) -> Result<Self, String> {
        match argument.trim() {
            "" => Ok(Self::Show),
            "clear" => Ok(Self::Set(None)),
            text => {
                let Some(("set", role)) = text.split_once(char::is_whitespace) else {
                    return Err(CONTEXT_ROLE_USAGE.to_string());
                };
                ContextRole::new(role.trim())
                    .map(|role| Self::Set(Some(role)))
                    .map_err(str::to_owned)
            }
        }
    }
}

impl ChatWidget {
    pub(super) fn reject_invalid_context_role_command(&mut self, args: &str) -> bool {
        match ContextRoleCommand::parse(args) {
            Ok(_) => false,
            Err(error) => {
                self.add_error_message(error);
                true
            }
        }
    }

    pub(super) fn dispatch_context_role(&mut self, argument: &str) {
        let command = match ContextRoleCommand::parse(argument) {
            Ok(command) => command,
            Err(error) => {
                self.add_error_message(error);
                return;
            }
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
