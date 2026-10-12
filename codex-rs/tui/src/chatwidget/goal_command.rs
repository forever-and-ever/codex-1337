use super::ChatWidget;
use super::LocalImageAttachment;
use super::QueuedInputAction;
use super::UserMessage;
use super::slash_dispatch::SlashCommandDispatchSource;
use crate::app_event::AppEvent;
use crate::app_event::ThreadGoalSetMode;
use crate::goal_files::GoalDraft;
use codex_app_server_protocol::ThreadGoalStatus;
use codex_features::Feature;
use codex_protocol::user_input::ByteRange;
use codex_protocol::user_input::TextElement;
use strum_macros::EnumString;

pub(crate) const GOAL_USAGE: &str = "Usage: /goal [set <objective>|clear|edit|pause|resume]";
pub(super) const GOAL_USAGE_HINT: &str = "Example: /goal set improve benchmark coverage";
pub(crate) const GOAL_DESCRIPTION: &str = "view the goal; /goal set <objective> sets or replaces it";

#[derive(EnumString)]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
enum GoalVerb {
    Set,
    Clear,
    Edit,
    Pause,
    Resume,
}

impl GoalVerb {
    fn parse_args(args: &str) -> Result<(Self, &str), String> {
        let trimmed = args.trim();
        let (verb, payload) = trimmed
            .split_once(char::is_whitespace)
            .map_or((trimmed, ""), |(verb, rest)| (verb, rest.trim_start()));
        let command = verb
            .parse::<Self>()
            .map_err(|_| format!("Unknown /goal command '{verb}'. {GOAL_USAGE}"))?;
        match command {
            Self::Set if payload.is_empty() => Err("Usage: /goal set <objective>".to_string()),
            Self::Set => Ok((command, payload)),
            _ if !payload.is_empty() => Err(GOAL_USAGE.to_string()),
            _ => Ok((command, payload)),
        }
    }
}

impl ChatWidget {
    pub(super) fn reject_invalid_goal_command(&mut self, args: &str) -> bool {
            match GoalVerb::parse_args(args) {
                Ok(_) => false,
                Err(error) => {
                    self.add_error_message(error);
                    true
                }
            }
        }

    pub(super) fn dispatch_goal_command(
        &mut self,
        args: String,
        text_elements: Vec<TextElement>,
        pending_pastes: Vec<(String, String)>,
        local_images: Vec<LocalImageAttachment>,
        remote_image_urls: Vec<String>,
        source: SlashCommandDispatchSource,
    ) {
        if !self.config.features.enabled(Feature::Goals) {
            if source == SlashCommandDispatchSource::Live {
                self.clear_live_goal_submission();
            }
            return;
        }

        // TELOS(explicit-command-verbs): queued inputs must pass the same boundary as live input.
        let (command, payload) = match GoalVerb::parse_args(&args) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.add_error_message(error);
                return;
            }
        };
        match command {
            GoalVerb::Set => {}
            GoalVerb::Edit => {
                self.app_event_tx.send(AppEvent::OpenThreadGoalEditor {
                    thread_id: self.thread_id,
                });
                if source == SlashCommandDispatchSource::Live {
                    self.clear_live_goal_submission();
                }
                return;
            }
            GoalVerb::Clear | GoalVerb::Pause | GoalVerb::Resume => {
                let Some(thread_id) = self.thread_id else {
                    self.add_info_message(
                        GOAL_USAGE.to_string(),
                        Some("The session must start before you can change a goal.".to_string()),
                    );
                    if source == SlashCommandDispatchSource::Live {
                        self.clear_live_goal_submission();
                    }
                    return;
                };
                match command {
                    GoalVerb::Clear => {
                        self.app_event_tx.send(AppEvent::ClearThreadGoal { thread_id });
                    }
                    GoalVerb::Pause => {
                        self.app_event_tx.send(AppEvent::SetThreadGoalStatus {
                            thread_id,
                            status: ThreadGoalStatus::Paused,
                        });
                    }
                    GoalVerb::Resume => {
                        self.app_event_tx.send(AppEvent::SetThreadGoalStatus {
                            thread_id,
                            status: ThreadGoalStatus::Active,
                        });
                    }
                    _ => unreachable!(),
                }
                self.append_message_history_entry(format!("/goal {}", args.trim()));
                if source == SlashCommandDispatchSource::Live {
                    self.clear_live_goal_submission();
                }
                return;
            }
        }

        let payload_offset = args.trim_end().len() - payload.len();
        let text_elements = Self::slash_command_args_elements(payload, payload_offset, &text_elements);
        let draft = GoalDraft {
            objective: payload.to_string(),
            text_elements,
            pending_pastes,
            local_images,
            remote_image_urls,
        };
        let Some(thread_id) = self.thread_id else {
            if source == SlashCommandDispatchSource::Live {
                const GOAL_PREFIX: &str = "/goal set ";
                let text_elements = draft
                    .text_elements
                    .into_iter()
                    .map(|element| {
                        element.map_range(|range| ByteRange {
                            start: range.start + GOAL_PREFIX.len(),
                            end: range.end + GOAL_PREFIX.len(),
                        })
                    })
                    .collect();
                self.queue_user_message_with_options(
                    UserMessage {
                        text: format!("{GOAL_PREFIX}{}", draft.objective),
                        local_images: draft.local_images,
                        remote_image_urls: draft.remote_image_urls,
                        text_elements,
                        mention_bindings: Vec::new(),
                    },
                    QueuedInputAction::ParseSlash,
                    draft.pending_pastes,
                );
                self.clear_live_goal_submission();
            } else {
                self.add_info_message(
                    GOAL_USAGE.to_string(),
                    Some("The session must start before you can set a goal.".to_string()),
                );
            }
            return;
        };
        let history_objective = draft.objective.clone();
        self.app_event_tx.send(AppEvent::SetThreadGoalDraft {
            thread_id,
            draft,
            mode: ThreadGoalSetMode::ConfirmIfExists,
        });
        self.append_message_history_entry(format!("/goal set {history_objective}"));
        if source == SlashCommandDispatchSource::Live {
            self.clear_live_goal_submission();
        }
    }
}
