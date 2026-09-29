//! Input retained independently of rendering and process-local history-cell identities.

use crate::chatwidget::ThreadInputState;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;

pub const PROTOCOL: &str = "codex-ui-reload-v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct TerminalHints {
    pub(crate) colors: Option<crate::terminal_probe::DefaultColors>,
    pub(crate) enhanced_keys: Option<bool>,
    pub(crate) terminal_app_over_ssh: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedInput {
    pub(crate) primary: Option<ThreadId>,
    pub(crate) overview: bool,
    pub(crate) draft: Option<ThreadInputState>,
    pub(crate) kill_buffer: crate::bottom_pane::KillBufferSnapshot,
    pub(crate) terminal: Option<TerminalHints>,
    pub(crate) cwd: Option<AbsolutePathBuf>,
    pub(crate) threads: HashMap<ThreadId, ThreadInputState>,
    pub(crate) detached: HashMap<ThreadId, ThreadInputState>,
}
