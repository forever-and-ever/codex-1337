//! A completed response is addressed to its item, not whichever thread is active.

use crate::history_cell::HistoryCell;
use codex_protocol::ThreadId;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct PreparedAssistant {
    pub(crate) markdown: String,
    pub(crate) source: String,
    pub(crate) branch_cwd: Option<std::path::PathBuf>,
}

#[derive(Debug)]
pub(crate) struct PreparedStream {
    pub(crate) assistant: Option<PreparedAssistant>,
    pub(crate) thread_id: ThreadId,
    pub(crate) item_id: String,
    pub(crate) cell: Arc<dyn HistoryCell>,
}
