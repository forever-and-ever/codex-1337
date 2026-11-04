//! A tool's outcome is independent of the affected agent's status.

use super::AgentMetadata;
use super::SpawnRequestSummary;
use super::close_end;
use super::collab_event;
use super::first_agent_state;
use super::interaction_end;
use super::parse_thread_id;
use super::resume_begin;
use super::resume_end;
use super::spawn_end;
use super::spawn_request_summary;
use super::status_summary_line;
use super::sub_agent_activity_title;
use super::wait::AgentPresentation;
use super::wait::AgentToolHistoryCell;
use super::wait::WaitPresentation;
use super::wait::WaitState;
use super::waiting_end;
use crate::history_cell::HistoryCell;
use codex_app_server_protocol::CollabAgentTool;
use codex_app_server_protocol::CollabAgentToolCallStatus;
use codex_app_server_protocol::SubAgentActivityKind;
use codex_app_server_protocol::ThreadItemPayload;
use codex_protocol::ThreadId;

pub(super) fn tool_call_history_cell(
    item: &ThreadItemPayload,
    cached_spawn_request: Option<&SpawnRequestSummary>,
    mut agent_metadata: impl FnMut(ThreadId) -> AgentMetadata,
) -> Option<AgentToolHistoryCell> {
    let ThreadItemPayload::CollabAgentToolCall {
        id,
        tool,
        status,
        receiver_thread_ids,
        prompt,
        agents_states,
        wait_completion,
        ..
    } = item
    else {
        return None;
    };
    let first_receiver = receiver_thread_ids
        .first()
        .and_then(|id| parse_thread_id(id));
    let prompt = prompt.as_deref().unwrap_or_default();
    let (presentation, captured) = match tool {
        CollabAgentTool::Wait => {
            let state = WaitState::from_lifecycle(status, wait_completion.as_ref());
            // Running waits are client decoration; exact tool captures remain independent.
            if state.is_running() {
                return None;
            }
            let captured = waiting_end(receiver_thread_ids, agents_states, &mut agent_metadata);
            (
                AgentPresentation::Wait(WaitPresentation {
                    id: id.clone(),
                    state,
                }),
                captured,
            )
        }
        tool => {
            let notice = NoticePresentation {
                id: id.clone(),
                operation: NoticeOperation::from_tool(tool)?,
                status: status.clone(),
            };
            if notice.is_running() && notice.operation != NoticeOperation::Resume {
                return None;
            }
            let fallback_spawn_request = spawn_request_summary(item);
            let spawn_request = cached_spawn_request.or(fallback_spawn_request.as_ref());
            let captured = if let Some(receiver) = first_receiver {
                match notice.operation {
                    NoticeOperation::Spawn => spawn_end(
                        notice.heading(),
                        receiver,
                        prompt,
                        spawn_request,
                        &mut agent_metadata,
                    ),
                    NoticeOperation::SendInput => {
                        interaction_end(notice.heading(), receiver, prompt, &mut agent_metadata)
                    }
                    NoticeOperation::Resume if notice.is_running() => {
                        resume_begin(receiver, &mut agent_metadata)
                    }
                    NoticeOperation::Resume => {
                        let detail = match first_agent_state(receiver_thread_ids, agents_states) {
                            Some(state) => status_summary_line(Some(state), ""),
                            None => match notice.status {
                                CollabAgentToolCallStatus::Failed => {
                                    status_summary_line(None, "Agent resume failed")
                                }
                                CollabAgentToolCallStatus::Interrupted => {
                                    "Agent resume interrupted".into()
                                }
                                CollabAgentToolCallStatus::Completed
                                | CollabAgentToolCallStatus::InProgress => {
                                    "Agent status unavailable".into()
                                }
                            },
                        };
                        resume_end(notice.heading(), receiver, detail, &mut agent_metadata)
                    }
                    NoticeOperation::Close => {
                        close_end(notice.heading(), receiver, &mut agent_metadata)
                    }
                }
            } else {
                unavailable_recipient(&notice, prompt, spawn_request)
            };
            (AgentPresentation::Notice(notice), captured)
        }
    };
    Some(AgentToolHistoryCell {
        timestamp: None,
        captured,
        presentation,
    })
}

fn unavailable_recipient(
    notice: &NoticePresentation,
    prompt: &str,
    spawn_request: Option<&SpawnRequestSummary>,
) -> crate::history_cell::PlainHistoryCell {
    let title = super::title_with_agent(
        notice.heading(),
        super::AgentLabel {
            thread_id: None,
            nickname: None,
            role: None,
        },
        spawn_request,
    );
    let mut details = vec!["Recipient identity unavailable".into()];
    details.extend(super::prompt_line(prompt));
    collab_event(title, details)
}

pub(super) fn sub_agent_activity_history_cell(
    item: &ThreadItemPayload,
) -> Option<Box<dyn HistoryCell>> {
    let ThreadItemPayload::SubAgentActivity {
        id,
        kind,
        agent_path,
        ..
    } = item
    else {
        return None;
    };
    let captured = collab_event(sub_agent_activity_title(*kind, agent_path), Vec::new());
    // Completed is an autonomous child-turn event, not the invocation that started it.
    Some(match kind {
        SubAgentActivityKind::Completed => Box::new(captured),
        SubAgentActivityKind::Started
        | SubAgentActivityKind::Interacted
        | SubAgentActivityKind::Interrupted => Box::new(AgentToolHistoryCell {
            timestamp: None,
            captured,
            presentation: AgentPresentation::SubAgentInvocation { id: id.clone() },
        }),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoticeOperation {
    Spawn,
    SendInput,
    Resume,
    Close,
}

impl NoticeOperation {
    pub(super) fn from_tool(tool: &CollabAgentTool) -> Option<Self> {
        match tool {
            CollabAgentTool::SpawnAgent => Some(Self::Spawn),
            CollabAgentTool::SendInput => Some(Self::SendInput),
            CollabAgentTool::ResumeAgent => Some(Self::Resume),
            CollabAgentTool::CloseAgent => Some(Self::Close),
            // V2 has its own display activities; these calls are analytics-only.
            CollabAgentTool::SendMessage
            | CollabAgentTool::FollowupTask
            | CollabAgentTool::InterruptAgent
            | CollabAgentTool::ListAgents
            | CollabAgentTool::Wait => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct NoticePresentation {
    pub(crate) id: String,
    pub(super) operation: NoticeOperation,
    pub(super) status: CollabAgentToolCallStatus,
}

impl NoticePresentation {
    pub(crate) fn is_running(&self) -> bool {
        self.status == CollabAgentToolCallStatus::InProgress
    }

    pub(super) fn heading(&self) -> &'static str {
        use CollabAgentToolCallStatus::Completed;
        use CollabAgentToolCallStatus::Failed;
        use CollabAgentToolCallStatus::InProgress;
        use CollabAgentToolCallStatus::Interrupted;
        use NoticeOperation::Close;
        use NoticeOperation::Resume;
        use NoticeOperation::SendInput;
        use NoticeOperation::Spawn;
        match (self.operation, &self.status) {
            (Spawn, InProgress) => "Spawning",
            (Spawn, Completed) => "Spawned",
            (Spawn, Failed) => "Failed to spawn",
            (Spawn, Interrupted) => "Spawn interrupted for",
            (SendInput, InProgress) => "Sending input to",
            (SendInput, Completed) => "Sent input to",
            (SendInput, Failed) => "Failed to send input to",
            (SendInput, Interrupted) => "Input interrupted for",
            (Resume, InProgress) => "Resuming",
            (Resume, Completed) => "Resumed",
            (Resume, Failed) => "Failed to resume",
            (Resume, Interrupted) => "Resume interrupted for",
            (Close, InProgress) => "Closing",
            (Close, Completed) => "Closed",
            (Close, Failed) => "Failed to close",
            (Close, Interrupted) => "Close interrupted for",
        }
    }
}
