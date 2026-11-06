//! Converting the remaining plain and single-frame rollouts to the framed encoding.
//!
//! Readers keep both older encodings until a census counts none of either; until then a rollout
//! has one of three encodings, and the framed one is canonical.

use std::fs::File;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::ARCHIVED_SESSIONS_SUBDIR;
use crate::SESSIONS_SUBDIR;
use crate::WriterLockCoordinator;
use crate::compression;
use crate::framed;
use crate::thread_id_from_rollout_path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RolloutEncoding {
    Plain,
    /// Upstream's whole-file zstd encoding: decodable, but neither seekable nor appendable.
    SingleFrame,
    Framed,
}

/// Local rollout files per encoding, active and archived.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RolloutCensus {
    pub plain: usize,
    pub single_frame: usize,
    pub framed: usize,
}

impl RolloutCensus {
    pub fn of(rollouts: &[(PathBuf, RolloutEncoding)]) -> Self {
        let mut census = Self::default();
        for (_, encoding) in rollouts {
            match encoding {
                RolloutEncoding::Plain => census.plain += 1,
                RolloutEncoding::SingleFrame => census.single_frame += 1,
                RolloutEncoding::Framed => census.framed += 1,
            }
        }
        census
    }

    /// Rollouts still needing `codex rollouts compress`.
    pub fn unconverted(&self) -> usize {
        self.plain + self.single_frame
    }
}

enum ConversionOutcome {
    Converted {
        framed_path: PathBuf,
    },
    /// A live writer holds the thread; a later run converts it.
    Busy,
    AlreadyFramed,
}

/// What one conversion pass did, sized by physical bytes.
#[derive(Debug, Default)]
pub struct ConversionTally {
    pub converted: usize,
    /// Rollouts skipped because a live writer holds their thread.
    pub busy: usize,
    pub failed: Vec<(PathBuf, io::Error)>,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

/// Every local rollout file with its encoding, active and archived. Blocking.
pub fn local_rollouts(codex_home: &Path) -> io::Result<Vec<(PathBuf, RolloutEncoding)>> {
    let mut rollouts = Vec::new();
    for root in [SESSIONS_SUBDIR, ARCHIVED_SESSIONS_SUBDIR] {
        collect(codex_home.join(root).as_path(), &mut rollouts)?;
    }
    rollouts.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(rollouts)
}

fn collect(directory: &Path, rollouts: &mut Vec<(PathBuf, RolloutEncoding)>) -> io::Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect(path.as_path(), rollouts)?;
            continue;
        }
        let is_rollout = file_type.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(compression::parse_rollout_file_name)
                .is_some();
        if is_rollout {
            let encoding = encoding_of(path.as_path())?;
            rollouts.push((path, encoding));
        }
    }
    Ok(())
}

pub fn encoding_of(path: &Path) -> io::Result<RolloutEncoding> {
    if !compression::is_compressed_rollout_path(path) {
        return Ok(RolloutEncoding::Plain);
    }
    let file = File::open(path)?;
    // An empty compressed file is a framed rollout whose writer has not written its hint yet.
    if file.metadata()?.len() == 0 || framed::is_framed(&file)? {
        Ok(RolloutEncoding::Framed)
    } else {
        Ok(RolloutEncoding::SingleFrame)
    }
}

/// Converts every unconverted rollout in `rollouts` (as listed by [`local_rollouts`]) to the
/// framed encoding, each under its thread's writer lock. Blocking.
pub fn convert_rollouts(
    codex_home: &Path,
    rollouts: Vec<(PathBuf, RolloutEncoding)>,
) -> ConversionTally {
    let coordinator = Arc::new(WriterLockCoordinator::new(codex_home));
    let mut tally = ConversionTally::default();
    for (path, encoding) in rollouts {
        if encoding == RolloutEncoding::Framed {
            continue;
        }
        let before = file_len(path.as_path());
        match convert_rollout(&coordinator, path.as_path()) {
            Ok(ConversionOutcome::Converted { framed_path }) => {
                tally.converted += 1;
                tally.bytes_before += before;
                tally.bytes_after += file_len(framed_path.as_path());
            }
            Ok(ConversionOutcome::Busy) => tally.busy += 1,
            Ok(ConversionOutcome::AlreadyFramed) => {}
            Err(err) => tally.failed.push((path, err)),
        }
    }
    tally
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// Converts one plain or single-frame rollout to the framed encoding under its thread's writer
/// lock, preserving its content byte for byte and its access metadata. Blocking.
fn convert_rollout(
    coordinator: &Arc<WriterLockCoordinator>,
    path: &Path,
) -> io::Result<ConversionOutcome> {
    let thread_id = thread_id_from_rollout_path(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a rollout file name", path.display()),
        )
    })?;
    let _writer_lock = match coordinator.acquire(thread_id) {
        Ok(lock) => lock,
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(ConversionOutcome::Busy),
        Err(err) => return Err(err),
    };
    if encoding_of(path)? == RolloutEncoding::Framed {
        return Ok(ConversionOutcome::AlreadyFramed);
    }
    let framed_path = compression::convert_rollout_blocking(path)?;
    Ok(ConversionOutcome::Converted { framed_path })
}

/// Logs how many rollouts still need conversion, off the startup path.
pub fn spawn_rollout_census(codex_home: PathBuf) {
    tokio::task::spawn_blocking(move || match local_rollouts(codex_home.as_path()) {
        Ok(rollouts) => {
            let census = RolloutCensus::of(&rollouts);
            if census.unconverted() == 0 {
                tracing::info!(framed = census.framed, "every local rollout is framed");
                return;
            }
            tracing::info!(
                plain = census.plain,
                single_frame = census.single_frame,
                framed = census.framed,
                "local rollouts await `codex rollouts compress`"
            );
        }
        Err(err) => tracing::warn!("rollout census failed: {err}"),
    });
}

/// Runs one best-effort conversion pass off the async runtime, reporting through logs.
pub fn spawn_rollout_conversion(codex_home: PathBuf) {
    tokio::task::spawn_blocking(move || {
        let rollouts = match local_rollouts(codex_home.as_path()) {
            Ok(rollouts) => rollouts,
            Err(err) => {
                tracing::warn!("rollout conversion could not list rollouts: {err}");
                return;
            }
        };
        let tally = convert_rollouts(codex_home.as_path(), rollouts);
        for (path, err) in &tally.failed {
            tracing::warn!("failed to convert rollout {}: {err}", path.display());
        }
        tracing::info!(
            converted = tally.converted,
            busy = tally.busy,
            failed = tally.failed.len(),
            bytes_before = tally.bytes_before,
            bytes_after = tally.bytes_after,
            "rollout conversion pass finished"
        );
    });
}
