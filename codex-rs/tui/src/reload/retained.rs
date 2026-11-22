//! Input retained independently of rendering and process-local history-cell identities.

use crate::chatwidget::ThreadInputState;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;

pub const PROTOCOL: &str = "codex-ui-reload-v1";

pub(super) const LEGACY_CHECKPOINT: u32 = 1;
const UNSTARTED_CHECKPOINT: u32 = 2;
const CANONICAL_INPUT_CHECKPOINT: u32 = 3;
pub const READABLE_CHECKPOINT_VERSIONS: &[u32] = &[
    LEGACY_CHECKPOINT,
    UNSTARTED_CHECKPOINT,
    CANONICAL_INPUT_CHECKPOINT,
];

pub(super) fn checkpoint_version(checkpoint: &serde_json::Value) -> std::io::Result<u32> {
    let object = checkpoint
        .as_object()
        .ok_or_else(|| std::io::Error::other("UI checkpoint must be an object"))?;
    let canonical = |input: &serde_json::Value| input.get("input_queue").is_some();
    let canonical_input = object.get("draft").is_some_and(canonical)
        || object
            .get("unstarted")
            .and_then(|draft| draft.get("input"))
            .is_some_and(canonical)
        || ["threads", "detached"]
            .into_iter()
            .filter_map(|field| object.get(field).and_then(serde_json::Value::as_object))
            .flat_map(serde_json::Map::values)
            .any(canonical);
    // New hosts reject unreadable downgrade candidates before changing workers.
    // An already-running old host cannot learn this detector: its strict old
    // worker rejects v3 input, retaining the checkpoint for retry with a new UI.
    Ok(if canonical_input {
        CANONICAL_INPUT_CHECKPOINT
    } else if object.contains_key("unstarted") {
        UNSTARTED_CHECKPOINT
    } else {
        LEGACY_CHECKPOINT
    })
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RetainedSessionDraft {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) cwd: AbsolutePathBuf,
    #[serde(deserialize_with = "decode_input")]
    pub(crate) input: ThreadInputState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct TerminalHints {
    pub(crate) colors: Option<crate::terminal_probe::DefaultColors>,
    pub(crate) enhanced_keys: Option<bool>,
    pub(crate) terminal_app_over_ssh: Option<bool>,
    pub(crate) bracketed_paste: Option<bool>,
    pub(crate) alternate_scroll_enabled: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedInput {
    pub(crate) primary: Option<ThreadId>,
    pub(crate) overview: bool,
    #[serde(default, deserialize_with = "decode_draft")]
    pub(crate) draft: Option<ThreadInputState>,
    // Only draft-aware workers emit this extension. Older workers always hand off an existing
    // server thread; omitting it keeps their checkpoints and rollback path unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) unstarted: Option<RetainedSessionDraft>,
    pub(crate) kill_buffer: crate::bottom_pane::KillBufferSnapshot,
    pub(crate) terminal: Option<TerminalHints>,
    pub(crate) cwd: Option<AbsolutePathBuf>,
    #[serde(deserialize_with = "decode_threads")]
    pub(crate) threads: HashMap<ThreadId, ThreadInputState>,
    #[serde(deserialize_with = "decode_threads")]
    pub(crate) detached: HashMap<ThreadId, ThreadInputState>,
}

impl RetainedInput {
    pub(crate) fn prepare_cli(&self, cli: &mut crate::cli::Cli) {
        cli.prompt = None;
        cli.images.clear();
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

fn decode_input<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<ThreadInputState, D::Error> {
    ThreadInputState::decode_retained(serde_json::Value::deserialize(decoder)?)
        .map_err(serde::de::Error::custom)
}

fn decode_draft<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Option<ThreadInputState>, D::Error> {
    Option::<serde_json::Value>::deserialize(decoder)?
        .map(ThreadInputState::decode_retained)
        .transpose()
        .map_err(serde::de::Error::custom)
}

fn decode_threads<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<HashMap<ThreadId, ThreadInputState>, D::Error> {
    HashMap::<ThreadId, serde_json::Value>::deserialize(decoder)?
        .into_iter()
        .map(|(id, input)| ThreadInputState::decode_retained(input).map(|input| (id, input)))
        .collect::<serde_json::Result<_>>()
        .map_err(serde::de::Error::custom)
}
