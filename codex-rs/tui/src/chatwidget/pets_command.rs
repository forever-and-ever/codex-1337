use super::ChatWidget;
use crate::app_event::AppEvent;
use strum_macros::EnumString;

const PETS_USAGE: &str = "Usage: /pets [select <id>|disable]";
pub(crate) const PETS_DESCRIPTION: &str = "choose a pet with /pets select <id>, or /pets disable";

#[derive(EnumString)]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
enum PetVerb {
    Select,
    #[strum(serialize = "disable", serialize = "disabled", serialize = "hide", serialize = "hidden", serialize = "off", serialize = "none")]
    Disable,
}

impl PetVerb {
    fn parse_args(args: &str) -> Result<(Self, &str), &'static str> {
        let trimmed = args.trim();
        let (verb, payload) = trimmed
            .split_once(char::is_whitespace)
            .map_or((trimmed, ""), |(verb, rest)| (verb, rest.trim()));
        match verb.parse::<Self>() {
            Ok(Self::Select) if !payload.is_empty() => Ok((Self::Select, payload)),
            Ok(Self::Disable) if payload.is_empty() => Ok((Self::Disable, payload)),
            _ => Err(PETS_USAGE),
        }
    }
}

impl ChatWidget {
    pub(super) fn reject_invalid_pet_command(&mut self, args: &str) -> bool {
        match PetVerb::parse_args(args) {
            Ok(_) => false,
            Err(error) => {
                self.add_error_message(error.to_string());
                true
            }
        }
    }

    pub(super) fn dispatch_pet_command(&mut self, args: &str) {
        // TELOS(explicit-command-verbs): an unknown mode is never a pet id.
        match PetVerb::parse_args(args) {
            Ok((PetVerb::Select, pet_id)) => self.select_pet_by_id(pet_id.to_string()),
            Ok((PetVerb::Disable, _)) => self.app_event_tx.send(AppEvent::PetDisabled),
            Err(error) => self.add_error_message(error.to_string()),
        }
    }
}
