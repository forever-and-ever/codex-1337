//! Checkpoint-reader admission is separate from the stable host/worker transport.
//! A legacy probe only establishes support for checkpoints without extensions.
//! The opt-in reply is one canonical JSON array of strictly increasing positive
//! reader versions followed by a newline. Bound both its size and its lifetime.

use super::retained::LEGACY_CHECKPOINT;
use super::retained::PROTOCOL;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub(super) const BUDGET: Duration = Duration::from_secs(/*secs*/ 1);
const MAX_OUTPUT: u64 = 256;

pub(super) async fn admit(candidate: &Path, version: u32) -> io::Result<PathBuf> {
    let check = async {
        let candidate = tokio::fs::canonicalize(candidate).await?;
        let (status, bytes) = query(&candidate, &["--checkpoint-versions"]).await?;
        if status.success() {
            let versions: Vec<u32> = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            let mut canonical = serde_json::to_vec(&versions).map_err(io::Error::other)?;
            canonical.push(b'\n');
            if versions.first().is_none_or(|version| *version == 0)
                || versions.windows(2).any(|pair| pair[0] >= pair[1])
                || bytes != canonical
            {
                return Err(io::Error::other(
                    "Invalid UI checkpoint-version probe output",
                ));
            }
            if versions.contains(&version) {
                return Ok(candidate);
            }
        } else if version == LEGACY_CHECKPOINT {
            let (status, bytes) = query(&candidate, &[]).await?;
            if status.success() && bytes == format!("{PROTOCOL}\n").as_bytes() {
                return Ok(candidate);
            }
        }
        Err(io::Error::other(
            "Installed UI cannot read this checkpoint version",
        ))
    };
    tokio::time::timeout(BUDGET, check)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UI checkpoint probe timed out"))?
}

async fn query(candidate: &Path, options: &[&str]) -> io::Result<(ExitStatus, Vec<u8>)> {
    let mut child = tokio::process::Command::new(candidate)
        .args(["debug", "tui-reload-probe"])
        .args(options)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut bytes = Vec::new();
    let _ = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing UI probe output pipe"))?
        .take(MAX_OUTPUT + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > MAX_OUTPUT {
        return Err(io::Error::other(
            "UI checkpoint-version probe output is too large",
        ));
    }
    Ok((child.wait().await?, bytes))
}
