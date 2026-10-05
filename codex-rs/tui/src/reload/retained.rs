//! Input retained independently of rendering and process-local history-cell identities.

use crate::chatwidget::ThreadInputState;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;

pub const PROTOCOL: &str = "codex-ui-reload-v1";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RetainedSessionDraft {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) cwd: AbsolutePathBuf,
    pub(crate) input: ThreadInputState,
}

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
    // Only draft-aware workers emit this extension. Older workers always hand off an existing
    // server thread; omitting it keeps their checkpoints and rollback path unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) unstarted: Option<RetainedSessionDraft>,
    pub(crate) kill_buffer: crate::bottom_pane::KillBufferSnapshot,
    pub(crate) terminal: Option<TerminalHints>,
    pub(crate) cwd: Option<AbsolutePathBuf>,
    pub(crate) threads: HashMap<ThreadId, ThreadInputState>,
    pub(crate) detached: HashMap<ThreadId, ThreadInputState>,
}

impl RetainedInput {
    pub(crate) fn prepare_cli(&self, cli: &mut crate::cli::Cli) {
        cli.prompt = None;
        cli.images.clear();
        cli.worktree = false;
        if let Some(cwd) = &self.cwd {
            cli.cwd = Some(cwd.to_path_buf());
        }
        cli.agents_overview = self.overview;
        cli.resume_picker = false;
        cli.resume_last = false;
        cli.resume_session_id = self.primary.map(|thread| thread.to_string());
        cli.fork_picker = false;
        cli.fork_last = false;
        cli.fork_session_id = None;
        if self.primary.is_none()
            && let Some(draft) = &self.unstarted
        {
            cli.model = Some(draft.model.clone());
            cli.cwd = Some(draft.cwd.to_path_buf());
            cli.config_overrides.raw_overrides.push(format!(
                "model_provider={}",
                toml::Value::String(draft.provider.clone())
            ));
        }
    }
}
