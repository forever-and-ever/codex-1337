use std::io;
use std::path::Path;

use codex_history::QueuedInputReceipt;
use serde_json::Value;

use crate::RolloutItem;
use crate::compression::existing_rollout_path;
use crate::compression::read_rollout_lines;
use crate::decode_rollout_line;

/// Scans one canonical rollout generation. Legacy event types need not decode,
/// but malformed JSON or an invalid receipt-bearing input makes absence unknowable.
pub async fn read_queued_input_receipts(path: &Path) -> io::Result<Vec<QueuedInputReceipt>> {
    let path = existing_rollout_path(path).await.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("rollout {} is missing", path.display()),
        )
    })?;
    let receipts = read_rollout_lines(&path, |lines| {
        let mut receipts = Vec::new();
        for line in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line).map_err(invalid_history)?;
            if !value.is_object() {
                return Err(invalid_history("rollout record is not an object"));
            }
            if value.pointer("/metadata/queued_input").is_none() {
                continue;
            }
            let RolloutItem::ResponseItem(envelope) =
                decode_rollout_line(value).map_err(invalid_history)?.item
            else {
                return Err(invalid_history("queue receipt has no canonical input"));
            };
            let receipt = envelope
                .metadata
                .and_then(|metadata| metadata.queued_input)
                .ok_or_else(|| invalid_history("invalid canonical queue receipt"))?;
            receipts.push(receipt);
        }
        Ok(receipts)
    })
    .await?;
    if !receipts.is_empty() {
        // The prior worker can die after an intact receipt reaches the file but before its
        // sync. Recovery must establish durability too, before it authorizes queue cleanup.
        tokio::fs::File::open(&path).await?.sync_all().await?;
        #[cfg(unix)]
        sync_rollout_directories(&path).await?;
    }
    Ok(receipts)
}

#[cfg(unix)]
async fn sync_rollout_directories(path: &Path) -> io::Result<()> {
    let mut directories = path.ancestors().skip(1);
    for directory in directories.by_ref() {
        tokio::fs::File::open(directory).await?.sync_all().await?;
        if directory.file_name().is_some_and(|name| {
            name == crate::SESSIONS_SUBDIR || name == crate::ARCHIVED_SESSIONS_SUBDIR
        }) {
            break;
        }
    }
    if let Some(home) = directories.next() {
        tokio::fs::File::open(home).await?.sync_all().await?;
    }
    Ok(())
}

fn invalid_history(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
