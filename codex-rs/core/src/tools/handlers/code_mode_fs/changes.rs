use super::MAX_BYTES;
use crate::tools::context::ToolInvocation;
use codex_apply_patch::AppliedPatchChange;
use codex_apply_patch::AppliedPatchFileChange;
use codex_file_system::EnvironmentAccess;
use codex_protocol::items::FileChangeItem;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::PatchApplyStatus;
use codex_protocol::protocol::TurnDiffEvent;
use codex_utils_path_uri::PathUri;
use futures::StreamExt;
use std::collections::HashMap;
use std::io;
use std::time::Duration;

pub(super) enum WriteChange {
    Unchanged,
    Text(Box<AppliedPatchChange>),
    Unavailable,
}

impl WriteChange {
    pub(super) async fn capture(
        access: &dyn EnvironmentAccess,
        path: &PathUri,
        after: &[u8],
    ) -> Self {
        // Presentation may neither acquire broader read authority nor make a permitted write
        // depend on a readable, small, textual predecessor.
        let before = match snapshot(access, path).await {
            Ok(before) => Some(before),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return Self::Unavailable,
        };
        if before.as_deref() == Some(after) {
            return Self::Unchanged;
        }
        let Some(new_content) = text(after) else {
            return Self::Unavailable;
        };
        let change = match before {
            None => AppliedPatchFileChange::Add {
                content: new_content,
                overwritten_content: None,
            },
            Some(before) => {
                let Some(old_content) = text(&before) else {
                    return Self::Unavailable;
                };
                AppliedPatchFileChange::Update {
                    move_path: None,
                    old_content,
                    overwritten_move_content: None,
                    new_content,
                }
            }
        };
        Self::Text(Box::new(AppliedPatchChange {
            path: path.clone(),
            change,
        }))
    }

    pub(super) async fn publish(self, invocation: &ToolInvocation, environment_id: &str) {
        if matches!(self, Self::Unchanged) {
            return;
        }
        let (had_diff, unified_diff) = {
            let mut tracker = invocation.tracker.lock().await;
            let had_diff = tracker.has_unified_diff();
            match &self {
                Self::Text(change) => tracker.track_file_write(environment_id, change),
                Self::Unavailable => tracker.invalidate(),
                Self::Unchanged => unreachable!(),
            }
            (had_diff, tracker.get_unified_diff())
        };
        if let Self::Text(change) = self {
            let protocol_change = match change.change {
                AppliedPatchFileChange::Add { content, .. } => FileChange::Add { content },
                AppliedPatchFileChange::Update {
                    old_content,
                    new_content,
                    ..
                } => FileChange::Update {
                    unified_diff: similar::TextDiff::configure()
                        .timeout(Duration::from_millis(200))
                        .diff_lines(&old_content, &new_content)
                        .unified_diff()
                        .context_radius(3)
                        .to_string(),
                    move_path: None,
                },
                AppliedPatchFileChange::Delete { .. } => unreachable!("writes never delete"),
            };
            invocation
                .session
                .emit_turn_item_completed(
                    &invocation.turn,
                    TurnItem::FileChange(FileChangeItem {
                        id: invocation.call_id.clone(),
                        changes: HashMap::from([(change.path.to_path_buf(), protocol_change)]),
                        status: Some(PatchApplyStatus::Completed),
                        auto_approved: None,
                        stdout: None,
                        stderr: None,
                    }),
                )
                .await;
        }
        if had_diff || unified_diff.is_some() {
            invocation
                .session
                .send_event(
                    &invocation.turn,
                    EventMsg::TurnDiff(TurnDiffEvent {
                        unified_diff: unified_diff.unwrap_or_default(),
                    }),
                )
                .await;
        }
    }
}

fn text(bytes: &[u8]) -> Option<String> {
    (!bytes.contains(&0))
        .then(|| std::str::from_utf8(bytes).ok().map(str::to_owned))
        .flatten()
}

async fn snapshot(access: &dyn EnvironmentAccess, path: &PathUri) -> io::Result<Vec<u8>> {
    let mut stream = access.read_file_stream(path).await?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > MAX_BYTES - bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "diff snapshot exceeds 1 MiB",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
