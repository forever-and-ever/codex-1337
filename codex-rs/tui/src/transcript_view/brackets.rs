//! Whose block each history cell is: the bracket its rows hang from until a line names another
//! speaker. The client's own chrome hangs none.

use std::sync::Arc;

use codex_protocol::step_admission::ArrivalKind;

use super::EntryKind;
use super::feed;
use super::text::Joins;
use super::text::TextLayout;
use crate::exec_cell::ExecCall;
use crate::exec_cell::ExecCell;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::warpaint::Bracket;

pub(super) fn of_cell(cell: &dyn HistoryCell) -> Option<Bracket> {
    if let Some(entry) = feed::feed_entry(cell) {
        return Some(match entry.feed_kind() {
            EntryKind::Arrival(ArrivalKind::InterAgent) => Bracket::Message,
            EntryKind::Arrival(ArrivalKind::User) => Bracket::Prompt,
            EntryKind::Preamble => Bracket::Response,
            EntryKind::Arrival(ArrivalKind::Injection)
            | EntryKind::Alarm
            | EntryKind::Compaction
            | EntryKind::Farm => Bracket::Harness,
        });
    }
    let any = cell.as_any();
    if let Some(exec) = any.downcast_ref::<ExecCell>() {
        return Some(if exec.iter_calls().any(ExecCall::is_user_shell_command) {
            Bracket::Prompt
        } else {
            Bracket::Call
        });
    }
    let bracket = if any.is::<history_cell::UserHistoryCell>() {
        Bracket::Prompt
    } else if any.is::<history_cell::AgentMessageCell>()
        || any.is::<history_cell::AgentMarkdownCell>()
        || any.is::<history_cell::StreamingAgentTailCell>()
        || any.is::<history_cell::ProposedPlanCell>()
        || any.is::<history_cell::ProposedPlanStreamCell>()
        || any.is::<history_cell::StreamingPlanTailCell>()
        || any.is::<history_cell::RespondingCell>()
        || any.is::<history_cell::RequestUserInputResultCell>()
    {
        Bracket::Response
    } else if any.is::<history_cell::ThoughtCell>()
        || any.is::<history_cell::ReasoningSummaryCell>()
    {
        Bracket::Reasoning
    } else if any.is::<history_cell::DelegatedMessageCell>()
        || any.is::<crate::multi_agents::AgentToolHistoryCell>()
    {
        Bracket::Message
    } else if any.is::<history_cell::McpToolCallCell>()
        || any.is::<history_cell::DynamicToolCallCell>()
        || any.is::<history_cell::ToolItemCell>()
        || any.is::<history_cell::ToolInputCell>()
        || any.is::<history_cell::ToolOutputGroup>()
        || any.is::<history_cell::ToolOutputCell>()
        || any.is::<history_cell::CodeModeCell>()
        || any.is::<history_cell::CodeModeFactCell>()
        || any.is::<history_cell::CodeModeWaitCell>()
        || any.is::<history_cell::WebSearchCell>()
        || any.is::<history_cell::PatchHistoryCell>()
        || any.is::<history_cell::ViewImageHistoryCell>()
        || any.is::<history_cell::PlanUpdateCell>()
        || any.is::<history_cell::ComputerActivityCell>()
        || any.is::<history_cell::UnifiedExecInteractionCell>()
    {
        Bracket::Call
    } else if any.is::<history_cell::FinalMessageSeparator>()
        || any.is::<history_cell::SessionHeaderHistoryCell>()
        || any.is::<history_cell::SessionInfoCell>()
        || any.is::<history_cell::WorkStatusCell>()
    {
        return None;
    } else {
        Bracket::Harness
    };
    Some(bracket)
}

impl super::TranscriptView {
    /// The brackets the stroke of the entry at `index`, laid out as `layout`, may run on into
    /// across its edges: a streamed reply arrives as one entry per fragment, and an open header
    /// whose own entry shows nothing else heads the entry below it. A fragment takes the stroke of
    /// the stream above it on trust, so drawing it never prepares the entries above it.
    pub(super) fn joins(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        layout: &TextLayout,
    ) -> Option<Joins> {
        let above = if index > 0 && self.continues_stream(cells, index) {
            layout.top_bracket()
        } else {
            self.previous_visible(cells, index)
                .filter(|upper| self.indexed_seat(cells, *upper).header_only())
                .and_then(|upper| self.layout(cells, upper))
                .filter(|upper| layout.headed_by(upper))
                .and_then(|upper| upper.last_bracket())
        };
        let below = self.bracket_below(cells, index, layout);
        (above.is_some() || below.is_some()).then_some(Joins { above, below })
    }

    /// The speaker of the first words below `layout`, the entry at `index`, that its stroke runs on
    /// into: the body its open header heads, or its stream's next words, through blank fragments
    /// of that speaker.
    fn bracket_below(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        layout: &TextLayout,
    ) -> Option<Bracket> {
        let mut upper = index;
        let mut through = None;
        loop {
            let lower = self.next_nonempty(cells, upper + 1)?;
            let lower_layout = self.layout(cells, lower)?;
            let headed = upper == index && lower_layout.headed_by(layout);
            if !(headed || self.continues_stream(cells, lower)) {
                return None;
            }
            let blank = match lower_layout.first_bracket() {
                Some(bracket) => {
                    return through
                        .is_none_or(|through| through == bracket)
                        .then_some(bracket);
                }
                None => lower_layout.blank_bracket()?,
            };
            if through.is_some_and(|through| through != blank) {
                return None;
            }
            through = Some(blank);
            upper = lower;
        }
    }

    /// Whether the entry at `index` continues the stream drawn above it; the live tail is `cells.len()`.
    fn continues_stream(&self, cells: &[Arc<dyn HistoryCell>], index: usize) -> bool {
        cells.get(index).map_or(
            index == cells.len() && self.live.is_some() && self.live_continuation,
            |cell| cell.is_stream_continuation(),
        )
    }
}
