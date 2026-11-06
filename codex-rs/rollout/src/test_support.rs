//! Fixtures that model torn and foreign writes to a rollout in either encoding. They take no
//! writer lock: a live writer of the same rollout fails closed at its next write.

use std::io;
use std::path::Path;
use std::path::PathBuf;

/// Appends `bytes` verbatim, as their own closed frame in a framed rollout: a partial line stays
/// partial until a later append completes it, as in a plain rollout.
pub fn append_rollout_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let path = rollout_file(path);
    let path = path.as_path();
    if !crate::compression::is_compressed_rollout_path(path) {
        use std::io::Write;
        return std::fs::OpenOptions::new()
            .append(true)
            .open(path)?
            .write_all(bytes);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut writer = crate::framed::FramedWriter::open(path, file)?;
    writer.append(bytes, /*flush*/ true)?;
    writer.close_frame()
}

/// Replaces a rollout's content with `text`, keeping its encoding.
pub fn rewrite_rollout(path: &Path, text: &str) -> io::Result<()> {
    let path = rollout_file(path);
    if !crate::compression::is_compressed_rollout_path(path.as_path()) {
        return std::fs::write(path, text);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    crate::framed::reencode(text.as_bytes(), path.as_path(), file).map(drop)
}

/// The file behind a rollout's logical name: its plain file if one exists, else its framed one.
pub fn rollout_file(path: &Path) -> PathBuf {
    crate::compression::existing_rollout_with_metadata_sync(path)
        .map_or_else(|| path.to_path_buf(), |(path, _)| path)
}
