//! Farm input is prepared without consuming the rich composer draft. Acceptance or
//! explicit discard clears only the unchanged draft, and a restore fills only a composer
//! that newer input does not occupy; later edits are never overwritten.

use super::AppEvent;
use super::ChatWidget;
use super::PathBuf;
use super::SelectionItem;
use super::SelectionViewParams;
use super::ShellEscapePolicy;
use super::SlashCommand;
use super::ThreadComposerState;
use super::ThreadInputState;
use super::UserInput;
use super::UserMessageHistoryRecord;
use super::UserMessageSource;
use super::app_server_text_elements;
use codex_app_server_protocol::ThreadFarmMode;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FarmDraft {
    composer: ThreadComposerState,
    cursor: usize,
    origin: FarmDraftOrigin,
}

#[derive(Debug, Clone, PartialEq)]
enum FarmDraftOrigin {
    Composer,
    WorkOrder,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FarmSubmission {
    pub(crate) draft: FarmDraft,
    pub(crate) input: Vec<UserInput>,
    pub(crate) task_name: Option<String>,
}

impl FarmSubmission {
    pub(crate) fn work_order(text: String, task_name: String) -> Self {
        Self {
            draft: FarmDraft {
                composer: ThreadComposerState {
                    text: text.clone(),
                    ..Default::default()
                },
                cursor: text.len(),
                origin: FarmDraftOrigin::WorkOrder,
            },
            input: vec![UserInput::Text {
                text,
                text_elements: vec![],
            }],
            task_name: Some(task_name),
        }
    }
}

impl ThreadInputState {
    pub(crate) fn clear_farm_draft(&mut self, draft: &FarmDraft) {
        if let Some(composer) = self.composer.as_mut()
            && draft.clear_unchanged(composer)
        {
            self.composer = None;
        }
    }
}

impl ChatWidget {
    pub(crate) fn submit_jira_triage(&mut self, text: String) -> bool {
        if !self.is_session_configured()
            || self.submission_settings_pending()
            || self.pending_image_submission.is_some()
            || self.active_side_conversation
        {
            return false;
        }
        self.submit_user_message_with_history_and_shell_escape_policy(
            text.into(),
            UserMessageHistoryRecord::UserMessageText,
            ShellEscapePolicy::Disallow,
            UserMessageSource::Prompt,
        )
        .0
    }

    fn farm_draft(&self) -> FarmDraft {
        let draft = self.bottom_pane.composer_draft_snapshot();
        FarmDraft {
            composer: ThreadComposerState {
                text: draft.text,
                shell_mode: Some(draft.shell_mode.unwrap_or(false)),
                local_images: draft.local_images,
                remote_image_urls: draft.remote_image_urls,
                text_elements: draft.text_elements,
                mention_bindings: draft.mention_bindings,
                pending_pastes: draft.pending_pastes,
            },
            cursor: draft.cursor,
            origin: FarmDraftOrigin::Composer,
        }
    }

    pub(crate) fn clear_farm_draft(&mut self, draft: &FarmDraft) {
        let mut composer = self.farm_draft().composer;
        if draft.clear_unchanged(&mut composer) {
            self.restore_composer_state(composer);
        }
        self.request_redraw();
    }

    /// Returns whether `draft` now fills the composer; newer input is never replaced.
    pub(crate) fn restore_farm_draft(&mut self, draft: &FarmDraft) -> bool {
        let mut current = self.farm_draft().composer;
        if matches!(current.text.trim(), "/farm" | "/farm_fresh") {
            current.text.clear();
        }
        if current.has_content() && current != draft.composer {
            return false;
        }
        self.restore_composer_state(draft.composer.clone());
        self.bottom_pane.set_composer_cursor(draft.cursor);
        self.request_redraw();
        true
    }

    pub(super) fn dispatch_farm(&mut self, command: SlashCommand) {
        if self.is_parent_owned_thread() || self.active_side_conversation {
            self.add_error_message("Return to the main conversation before farming a task.".into());
            return;
        }
        let Some(thread_id) = self.thread_id else {
            self.add_error_message(
                "Wait for the main conversation to start before farming a task.".into(),
            );
            return;
        };
        self.bottom_pane.flush_composer_paste_burst();
        let draft = self.farm_draft();
        if crate::bottom_pane::prompt_args::parse_slash_name(&draft.composer.text)
            .is_some_and(|(_, args, _)| args.trim().is_empty())
        {
            self.app_event_tx
                .send(AppEvent::OpenFarmRecovery { thread_id });
            return;
        }
        if self.snapshot_local_images && !draft.composer.local_images.is_empty() {
            self.add_error_message("Farm cannot send local image attachments to a remote server yet; remove them or use an inline image.".into());
            return;
        }
        let prepared = self
            .bottom_pane
            .prepare_inline_args_submission(/*record_history*/ false);
        let local_images = self
            .bottom_pane
            .take_recent_submission_images_with_placeholders();
        let remote_images = self.take_remote_image_urls();
        let bindings = self.bottom_pane.take_recent_submission_mention_bindings();
        self.restore_composer_state(draft.composer.clone());
        self.bottom_pane.set_composer_cursor(draft.cursor);
        let Some((text, elements)) = prepared else {
            self.add_error_message(format!("Usage: /{} <task>", command.command()));
            return;
        };
        if text.trim().is_empty() {
            self.add_error_message(format!("Usage: /{} <task>", command.command()));
            return;
        }
        let mut input: Vec<UserInput> = remote_images
            .into_iter()
            .map(|url| UserInput::Image {
                image: codex_app_server_protocol::ImageReference::Inline { url },
                detail: None,
            })
            .collect();
        input.extend(local_images.into_iter().map(|image| UserInput::LocalImage {
            path: image.path,
            detail: None,
        }));
        input.push(UserInput::Text {
            text,
            text_elements: app_server_text_elements(&elements),
        });
        for binding in bindings {
            let skill_path = binding
                .path
                .strip_prefix("skill://")
                .map(PathBuf::from)
                .or_else(|| {
                    self.bottom_pane.skills().and_then(|skills| {
                        skills
                            .iter()
                            .find(|skill| skill.path.as_str() == binding.path)
                            .and_then(|skill| skill.path.to_inferred_abs_path())
                            .map(|path| path.to_path_buf())
                    })
                });
            input.push(if let Some(path) = skill_path {
                UserInput::Skill {
                    name: binding.mention,
                    path,
                }
            } else {
                UserInput::Mention {
                    name: binding.mention,
                    path: binding.path,
                }
            });
        }
        self.app_event_tx.send(AppEvent::Farm {
            thread_id,
            mode: match command {
                SlashCommand::Farm => ThreadFarmMode::FullHistory,
                SlashCommand::FarmFresh => ThreadFarmMode::Fresh,
                _ => unreachable!("farm dispatch only"),
            },
            submission: FarmSubmission {
                draft,
                input,
                task_name: None,
            },
        });
    }

    pub(crate) fn open_farm_recovery(&mut self, request_id: String) {
        let reconcile_id = request_id.clone();
        let restore_id = request_id.clone();
        let items = vec![
            SelectionItem {
                name: "Check receipt".into(),
                description: Some("Ask about the same request; never create a second task.".into()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::ReconcileFarm {
                        request_id: reconcile_id.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Discard pending request".into(),
                description: Some(
                    "A child may still be running. Allow new tasks without resending this one."
                        .into(),
                ),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::DiscardFarm {
                        request_id: request_id.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Restore submitted draft".into(),
                description: Some(
                    "Restore into an empty composer; keep the same pending request.".into(),
                ),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::RestoreFarmDraft {
                        request_id: restore_id.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Farm receipt missing".into()),
            items,
            ..Default::default()
        });
        self.request_redraw();
    }
}

impl FarmDraft {
    fn clear_unchanged(&self, composer: &mut ThreadComposerState) -> bool {
        if self.origin != FarmDraftOrigin::Composer || *composer != self.composer {
            return false;
        }
        *composer = ThreadComposerState::default();
        true
    }
}
