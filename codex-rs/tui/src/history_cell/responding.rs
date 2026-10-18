//! A response's stable place in the transcript while its complete block is prepared.

use super::ActivityDisclosure;
use super::AgentMessageCell;
use super::HistoryCell;
use super::Line;
use super::ProposedPlanStreamCell;
use super::Stylize;

#[derive(Debug)]
pub(crate) struct RespondingCell {
    pub(crate) item_id: String,
}

impl HistoryCell for RespondingCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.raw_lines()
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![crate::render::header::paint_work(
            "Responding".bold().into(),
            true,
        )]
    }

    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        None
    }
}

pub(crate) fn streaming_item_id(cell: &dyn HistoryCell) -> Option<&str> {
    let any = cell.as_any();
    if let Some(response) = any.downcast_ref::<RespondingCell>() {
        return Some(&response.item_id);
    }
    if let Some(message) = any.downcast_ref::<AgentMessageCell>() {
        return message.item_id.as_deref();
    }
    any.downcast_ref::<ProposedPlanStreamCell>()
        .and_then(|plan| plan.item_id.as_deref())
}
