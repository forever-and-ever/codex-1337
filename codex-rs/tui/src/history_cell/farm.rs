use super::HistoryCell;
use crate::transcript_view::FeedEntry;
use crate::transcript_view::HeaderDisclosure;
use codex_protocol::ThreadId;
use ratatui::text::Line;

#[derive(Debug, Clone)]
pub(crate) struct FarmReceipt {
    pub(crate) thread_id: ThreadId,
    pub(crate) request_id: String,
    pub(crate) message: String,
}

impl HistoryCell for FarmReceipt {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![self.feed_title()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![self.message.clone().into()]
    }
}

// A human-started farm is independent of the model step running when its RPC completes.
impl FeedEntry for FarmReceipt {
    fn feed_id(&self) -> String {
        format!("farm:{}", self.request_id)
    }

    fn feed_disclosure(&self) -> HeaderDisclosure {
        HeaderDisclosure::FixedOpen
    }

    fn feed_title(&self) -> Line<'static> {
        Line::styled(self.message.clone(), crate::render::header::style())
    }
}
