//! Agent waits retain one named lifecycle; captured outputs are independent evidence.

use crate::history_cell::ActivityDisclosure;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryTimestamp;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::activity_parts::ActivityHeader;
use crate::history_cell::output_provenance::OutputProvenance;
use crate::history_cell::output_provenance::mark_lines;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use codex_app_server_protocol::CollabAgentToolCallStatus;
use codex_protocol::agent_wait::AgentWaitCompletion;
use codex_protocol::agent_wait::AgentWaitOutcome;
use ratatui::text::Line;

#[derive(Debug)]
pub(crate) struct AgentToolHistoryCell {
    pub(crate) timestamp: Option<HistoryTimestamp>,
    pub(crate) captured: PlainHistoryCell,
    pub(super) presentation: AgentPresentation,
}

#[derive(Debug)]
pub(super) enum AgentPresentation {
    Wait(WaitPresentation),
    Notice(super::notice::NoticePresentation),
    SubAgentInvocation { id: String },
}

#[derive(Debug)]
pub(crate) struct WaitPresentation {
    pub(crate) id: String,
    pub(crate) state: WaitState,
}

#[derive(Debug)]
pub(crate) enum WaitState {
    Running,
    Completed(Option<AgentWaitCompletion>),
    Failed,
    Interrupted,
}

impl WaitState {
    pub(crate) fn from_lifecycle(
        status: &CollabAgentToolCallStatus,
        completion: Option<&AgentWaitCompletion>,
    ) -> Self {
        match status {
            CollabAgentToolCallStatus::InProgress => Self::Running,
            CollabAgentToolCallStatus::Completed => Self::Completed(completion.cloned()),
            CollabAgentToolCallStatus::Failed => Self::Failed,
            CollabAgentToolCallStatus::Interrupted => Self::Interrupted,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }

    fn heading(&self) -> String {
        let heading = match self {
            Self::Running => "Waiting for agent updates",
            Self::Completed(Some(completion)) => match completion.outcome {
                AgentWaitOutcome::TimedOut => {
                    "Waited for agent updates · no updates before timeout"
                }
                AgentWaitOutcome::MailboxActivity => "Waited for agent updates · updates available",
                AgentWaitOutcome::Steered => "Waited for agent updates · new input arrived",
            },
            Self::Completed(None) => "Waited for agent updates · outcome unavailable",
            Self::Failed => "Agent wait failed",
            Self::Interrupted => "Agent wait interrupted",
        };
        match self {
            Self::Completed(Some(completion)) => {
                format!("{heading} · {} ms", completion.elapsed_ms)
            }
            _ => heading.to_owned(),
        }
    }
}

impl AgentToolHistoryCell {
    fn wait(&self) -> Option<&WaitPresentation> {
        match &self.presentation {
            AgentPresentation::Wait(wait) => Some(wait),
            AgentPresentation::Notice(_) | AgentPresentation::SubAgentInvocation { .. } => None,
        }
    }

    pub(crate) fn invocation_status(&self) -> Option<String> {
        self.wait().map(|wait| match &wait.state {
            WaitState::Running => "waiting".into(),
            WaitState::Completed(None) => "outcome unavailable".into(),
            WaitState::Completed(Some(completion)) => {
                let outcome = match completion.outcome {
                    AgentWaitOutcome::TimedOut => "timed out",
                    AgentWaitOutcome::MailboxActivity => "updates available",
                    AgentWaitOutcome::Steered => "new input arrived",
                };
                format!("{outcome} · {} ms", completion.elapsed_ms)
            }
            WaitState::Failed => "failed".into(),
            WaitState::Interrupted => "interrupted".into(),
        })
    }

    pub(crate) fn resume_for(cell: &dyn HistoryCell) -> Option<&super::notice::NoticePresentation> {
        let cell = cell
            .as_any()
            .downcast_ref::<crate::history_cell::ToolItemCell>()
            .map_or(cell, |cell| cell.inner.as_ref());
        match &cell.as_any().downcast_ref::<Self>()?.presentation {
            AgentPresentation::Notice(notice)
                if notice.operation == super::notice::NoticeOperation::Resume =>
            {
                Some(notice)
            }
            _ => None,
        }
    }

    fn id(&self) -> &str {
        match &self.presentation {
            AgentPresentation::Wait(wait) => &wait.id,
            AgentPresentation::Notice(notice) => &notice.id,
            AgentPresentation::SubAgentInvocation { id } => id,
        }
    }

    pub(crate) fn wait_for(cell: &dyn HistoryCell) -> Option<&WaitPresentation> {
        let cell = cell
            .as_any()
            .downcast_ref::<crate::history_cell::ToolItemCell>()
            .map_or(cell, |cell| cell.inner.as_ref());
        cell.as_any().downcast_ref::<Self>()?.wait()
    }

    fn arguments(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = if let Some(wait) = self.wait() {
            plain_hyperlink_lines(
                std::iter::once(Line::from("  wrangler_agents.wait_agent"))
                    .chain(
                        self.captured
                            .display_lines(width)
                            .into_iter()
                            .skip(usize::from(!wait.state.is_running())),
                    )
                    .collect(),
            )
        } else {
            self.captured.nested_detail_lines(width)
        };
        mark_lines(&mut lines, OutputProvenance::Client);
        lines
    }
}

impl HistoryCell for AgentToolHistoryCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.timestamp
    }

    fn tool_call_ids(&self) -> Vec<&str> {
        vec![self.id()]
    }

    fn outer_call_id(&self) -> Option<&str> {
        Some(self.id())
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![match &self.presentation {
            AgentPresentation::Wait(wait) => format!("agent-wait:{}", wait.id),
            AgentPresentation::Notice(notice) => format!("agent-tool:{}", notice.id),
            AgentPresentation::SubAgentInvocation { id } => format!("agent-activity:{id}"),
        }]
    }

    fn activity_header(&self) -> Option<ActivityHeader> {
        match self.wait() {
            Some(wait) => Some(ActivityHeader::at_end(&super::title_text(
                wait.state.heading(),
            ))),
            None => self
                .captured
                .display_lines(u16::MAX)
                .first()
                .map(ActivityHeader::at_end),
        }
    }

    fn activity_disclosure(&self, width: u16) -> Option<ActivityDisclosure> {
        if self.wait().is_some() {
            Some(ActivityDisclosure::Generic)
        } else {
            self.captured
                .has_nested_details(width)
                .then_some(ActivityDisclosure::Generic)
        }
    }

    fn is_tool_entry(&self) -> bool {
        true
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self.wait() {
            Some(wait) => vec![super::title_text(wait.state.heading())],
            None => self.captured.display_lines(width),
        }
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = plain_hyperlink_lines(self.display_lines(width));
        if self.wait().is_some() {
            mark_lines(&mut lines, OutputProvenance::Client);
        }
        lines
    }

    fn nested_summary_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if self.wait().is_some() {
            self.display_hyperlink_lines(width)
        } else {
            self.captured.nested_summary_lines(width)
        }
    }

    fn activity_argument_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.arguments(width)
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.arguments(width)
    }

    fn nested_detail_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.arguments(width)
    }

    fn has_nested_details(&self, width: u16) -> bool {
        self.wait().is_some() || self.captured.has_nested_details(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        if self.wait().is_some() {
            Vec::new()
        } else {
            self.captured.raw_lines()
        }
    }
}
