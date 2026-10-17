//! A prepared final revision is addressed to the stream that produced it, not the active view.

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
pub(crate) enum RevisionKind {
    Rewrap,
    Final(Option<PreparedAssistant>),
}

#[derive(Debug)]
pub(crate) struct PreparedStream {
    pub(crate) kind: RevisionKind,
    pub(crate) thread_id: ThreadId,
    pub(crate) item_id: String,
    pub(crate) cell: Arc<dyn HistoryCell>,
}
