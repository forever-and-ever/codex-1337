use std::fs::File;
use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

mod access_metadata;
mod blocking_reader;
mod error_metrics;
mod path_metadata;
mod read_diagnostics;
mod read_metrics;

pub(crate) use blocking_reader::read_rollout_lines;
pub use path_metadata::existing_rollout_with_metadata_sync;
use read_metrics::ReadFailureSource;
use read_metrics::ReadMetrics;

const COMPRESSED_SUFFIX: &str = ".zst";
const MAX_NOT_FOUND_RETRIES: usize = 3;
const OPEN_ROLLOUT_LINE_READER_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Returns the modified time for the existing plain or compressed rollout file.
pub(crate) async fn file_modified_time(path: &Path) -> io::Result<Option<time::OffsetDateTime>> {
    Ok(path::existing_rollout_with_metadata(path)
        .await
        .and_then(|(_, metadata)| metadata.modified().ok())
        .map(time::OffsetDateTime::from))
}

/// Opens a rollout line reader that transparently handles plain `.jsonl` and `.jsonl.zst` files.
///
/// If the requested path disappears during a representation transition, this briefly retries
/// resolution so callers do not need to know which representation is on disk.
pub async fn open_rollout_line_reader(path: &Path) -> io::Result<RolloutLineReader> {
    open_line_reader(path, ReaderScope::Full).await
}

pub(crate) async fn open_rollout_head_reader(path: &Path) -> io::Result<RolloutLineReader> {
    open_line_reader(path, ReaderScope::Head).await
}

#[derive(Clone, Copy)]
enum ReaderScope {
    Head,
    Full,
}

async fn open_line_reader(path: &Path, scope: ReaderScope) -> io::Result<RolloutLineReader> {
    let started_at = Instant::now();
    let mut metrics = ReadMetrics::default();
    let result = async {
        for _ in 0..MAX_NOT_FOUND_RETRIES {
            match reader::open_once(path, &mut metrics, scope).await {
                Ok(reader) => return Ok(reader),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    tokio::time::sleep(OPEN_ROLLOUT_LINE_READER_RETRY_DELAY).await;
                }
                Err(err) => return Err(err),
            }
        }
        reader::open_once(path, &mut metrics, scope).await
    }
    .await;
    metrics.duration = started_at.elapsed();
    match result {
        Ok(inner) => Ok(RolloutLineReader { inner, metrics }),
        Err(err) => {
            metrics.failed("open", ReadFailureSource::Stream, &err);
            Err(err)
        }
    }
}

#[cfg(test)]
pub(crate) use path::compressed_rollout_path;

pub(crate) fn is_compressed_rollout_path(path: &Path) -> bool {
    path::is_compressed_rollout_path(path)
}

/// Resolves the file an append extends, off the async runtime; see
/// [`resolve_rollout_for_append_blocking`].
pub(crate) async fn resolve_rollout_for_append(
    path: &Path,
    writer_lock: std::sync::Arc<crate::WriterLockGuard>,
) -> io::Result<PathBuf> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let _writer_lock = writer_lock;
        resolve_rollout_for_append_blocking(path.as_path())
    })
    .await
    .map_err(io::Error::other)?
}

/// Resolves the file an append extends behind a rollout's logical `.jsonl` name: an existing plain
/// rollout stays plain until `codex rollouts compress` converts it, a framed one is extended in place,
/// a single-frame one is re-encoded as framed first, and a new rollout is framed from its first
/// line. The caller holds the rollout's writer lock.
pub(crate) fn resolve_rollout_for_append_blocking(path: &Path) -> io::Result<PathBuf> {
    let plain_path = plain_rollout_path(path);
    if plain_path.try_exists()? {
        return Ok(plain_path);
    }
    let compressed_path = path::compressed_rollout_path(plain_path.as_path());
    match crate::conversion::encoding_of(compressed_path.as_path()) {
        Ok(crate::RolloutEncoding::SingleFrame) => {
            convert_rollout_blocking(compressed_path.as_path())
        }
        Ok(_) => Ok(compressed_path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(compressed_path),
        Err(err) => Err(err),
    }
}

/// Decodes a zstd rollout stream. A single-frame rollout's window can be as large as its content
/// (`zstd --long=31` writes such frames), past the decoder's default 128 MiB limit, so this admits
/// the format's 64-bit maximum.
pub(crate) fn max_window_decoder<R: Read>(
    input: R,
) -> io::Result<zstd::stream::read::Decoder<'static, io::BufReader<R>>> {
    let mut decoder = zstd::stream::read::Decoder::new(input)?;
    decoder.window_log_max(31)?;
    Ok(decoder)
}

/// Re-encodes the plain or single-frame rollout at `source` as the framed `.jsonl.zst` beside it,
/// preserving its access metadata, then removes a plain source. Returns the framed path. The
/// caller holds the rollout's writer lock.
pub(crate) fn convert_rollout_blocking(source: &Path) -> io::Result<PathBuf> {
    let destination = path::compressed_rollout_path(source);
    let input = File::open(source)?;
    let temp = tempfile::Builder::new().prefix(".rollout-frame-").make_in(
        destination.parent().unwrap_or_else(|| Path::new(".")),
        |path| {
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            options.open(path)
        },
    )?;
    let decoded: Box<dyn Read> = if path::is_compressed_rollout_path(source) {
        Box::new(max_window_decoder(&input)?)
    } else {
        Box::new(&input)
    };
    // Fails unless the synced temp file decodes to exactly the bytes read from the source.
    crate::framed::reencode(
        io::BufReader::new(decoded),
        temp.path(),
        temp.as_file().try_clone()?,
    )?;
    access_metadata::copy(&input, temp.as_file())?;
    temp.as_file().sync_all()?;
    temp.persist(destination.as_path())
        .map_err(|err| err.error)?;
    #[cfg(unix)]
    if let Some(directory) = destination.parent() {
        File::open(directory)?.sync_all()?;
    }
    if source != destination {
        // The framed file is complete before the plain one goes; a crash between leaves both,
        // and readers and writers keep preferring the plain one until the next conversion.
        std::fs::remove_file(source)?;
    }
    Ok(destination)
}

/// Returns the plain `.jsonl` path for a plain or compressed rollout path.
pub fn plain_rollout_path(path: &Path) -> PathBuf {
    path::plain_rollout_path(path)
}

/// Parses a rollout file name, returning its plain `.jsonl` name when valid.
pub(crate) fn parse_rollout_file_name(name: &str) -> Option<&str> {
    file_name::parse_rollout_file_name(name)
}

/// A discovered rollout file, represented by exactly one physical path.
///
/// This keeps directory walkers from reimplementing the plain/compressed
/// precedence rules. The physical path may point at either `.jsonl` or
/// `.jsonl.zst`, while `plain_file_name` is always the canonical `.jsonl`
/// filename used for timestamp and id parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RolloutFile {
    path: PathBuf,
    plain_file_name: String,
}

impl RolloutFile {
    /// Creates a logical rollout file from a physical path found during discovery.
    ///
    /// Returns `None` for non-rollout names and for compressed siblings hidden by
    /// an existing plain `.jsonl` file.
    pub(crate) fn from_path(path: PathBuf) -> Option<Self> {
        let file_name = path.file_name().and_then(|name| name.to_str())?;
        let plain_file_name = file_name::parse_rollout_file_name(file_name)?.to_string();
        if path::should_skip_compressed_sibling(path.as_path()) {
            return None;
        }

        Some(Self {
            path,
            plain_file_name,
        })
    }

    /// Returns the physical path that should be opened for reads.
    pub(crate) fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Returns the canonical `.jsonl` filename for timestamp and id parsing.
    pub(crate) fn plain_file_name(&self) -> &str {
        self.plain_file_name.as_str()
    }

    /// Returns whether the physical path is the compressed representation.
    pub(crate) fn is_compressed(&self) -> bool {
        path::is_compressed_rollout_path(self.path.as_path())
    }

    /// Consumes the entry and returns the physical path that should be read.
    pub(crate) fn into_path(self) -> PathBuf {
        self.path
    }
}

/// Line-oriented rollout reader returned by [`open_rollout_line_reader`].
pub struct RolloutLineReader {
    inner: RolloutLineReaderInner,
    metrics: ReadMetrics,
}

enum RolloutLineReaderInner {
    Plain(tokio::io::Lines<tokio::io::BufReader<tokio::fs::File>>),
    Blocking(Option<BlockingLineReader>),
}

impl RolloutLineReader {
    /// Reads the next JSONL record from the rollout.
    pub async fn next_line(&mut self) -> io::Result<Option<String>> {
        let started_at = Instant::now();
        self.metrics.reached_eof = false;
        let mut failure_source = ReadFailureSource::Stream;
        let result = async {
            match &mut self.inner {
                RolloutLineReaderInner::Plain(lines) => lines.next_line().await,
                RolloutLineReaderInner::Blocking(slot) => {
                    let Some(mut reader) = slot.take() else {
                        failure_source = ReadFailureSource::ReaderBusy;
                        return Err(io::Error::other("compressed rollout reader is busy"));
                    };
                    let (line, reader) =
                        tokio::task::spawn_blocking(move || (reader.next().transpose(), reader))
                            .await
                            .map_err(|err| {
                                failure_source = ReadFailureSource::TaskJoin;
                                io::Error::other(err)
                            })?;
                    *slot = Some(reader);
                    line
                }
            }
        }
        .await;
        self.metrics.duration = self.metrics.duration.saturating_add(started_at.elapsed());
        match &result {
            Ok(line) => {
                self.metrics.reached_eof = line.is_none();
                self.metrics.read_any_line |= line.is_some();
            }
            Err(err) => self.metrics.failed("read", failure_source, err),
        }
        result
    }

    /// Keeps a compressed scan on one worker while retaining this reader's format and I/O metrics.
    pub(crate) async fn find_map<T: Send + 'static>(
        mut self,
        mut find: impl FnMut(&str) -> Option<T> + Send + 'static,
    ) -> io::Result<Option<T>> {
        let RolloutLineReaderInner::Blocking(Some(reader)) = self.inner else {
            while let Some(line) = self.next_line().await? {
                if let Some(found) = find(&line) {
                    return Ok(Some(found));
                }
            }
            return Ok(None);
        };
        blocking_reader::scan_lines(reader, self.metrics, move |lines| {
            for line in lines {
                if let Some(found) = find(&line?) {
                    return Ok(Some(found));
                }
            }
            Ok(None)
        })
        .await
    }
}

type BlockingLineReader = std::io::Lines<std::io::BufReader<Box<dyn Read + Send>>>;

/// Returns the existing rollout path, preferring the plain `.jsonl` file over
/// its `.jsonl.zst` compressed sibling.
pub async fn existing_rollout_path(path: &Path) -> Option<PathBuf> {
    path::existing_rollout_path(path).await
}

mod path {
    use std::ffi::OsStr;
    use std::fs::Metadata;
    use std::path::Path;
    use std::path::PathBuf;

    use super::COMPRESSED_SUFFIX;

    pub(crate) fn compressed_rollout_path(path: &Path) -> PathBuf {
        if is_compressed_rollout_path(path) {
            return path.to_path_buf();
        }
        let mut file_name = path
            .file_name()
            .map(OsStr::to_os_string)
            .unwrap_or_else(|| OsStr::new("rollout.jsonl").to_os_string());
        file_name.push(COMPRESSED_SUFFIX);
        path.with_file_name(file_name)
    }

    pub(super) fn plain_rollout_path(path: &Path) -> PathBuf {
        let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
            return path.to_path_buf();
        };
        let Some(plain_file_name) = file_name.strip_suffix(COMPRESSED_SUFFIX) else {
            return path.to_path_buf();
        };
        path.with_file_name(plain_file_name)
    }

    pub(super) fn is_compressed_rollout_path(path: &Path) -> bool {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.ends_with(".jsonl.zst"))
    }

    pub(super) fn should_skip_compressed_sibling(path: &Path) -> bool {
        is_compressed_rollout_path(path) && plain_rollout_path(path).exists()
    }

    pub(super) async fn existing_rollout_path(path: &Path) -> Option<PathBuf> {
        existing_rollout_with_metadata(path)
            .await
            .map(|(path, _)| path)
    }

    /// Resolves the plain rollout before its compressed sibling and retains the lookup metadata.
    ///
    /// Returning the metadata lets callers inspect the selected file without a second stat.
    pub(super) async fn existing_rollout_with_metadata(path: &Path) -> Option<(PathBuf, Metadata)> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || super::existing_rollout_with_metadata_sync(&path))
            .await
            .ok()
            .flatten()
    }
}

mod file_name {
    use super::COMPRESSED_SUFFIX;

    pub(super) fn parse_rollout_file_name(name: &str) -> Option<&str> {
        let name = name.strip_suffix(COMPRESSED_SUFFIX).unwrap_or(name);
        if name.starts_with("rollout-") && name.ends_with(".jsonl") {
            Some(name)
        } else {
            None
        }
    }
}

mod reader {
    use std::fs::File;
    use std::io;
    use std::io::BufRead;
    use std::io::Read;
    use std::path::Path;

    use super::ReadFailureSource;
    use super::ReadMetrics;
    use super::ReaderScope;
    use super::RolloutLineReaderInner;
    use super::path;
    use tokio::io::AsyncBufReadExt;

    pub(super) async fn open_once(
        path: &Path,
        metrics: &mut ReadMetrics,
        scope: ReaderScope,
    ) -> io::Result<RolloutLineReaderInner> {
        let path = path::existing_rollout_path(path)
            .await
            .unwrap_or_else(|| path.to_path_buf());
        if path::is_compressed_rollout_path(path.as_path()) {
            let (format, reader) = tokio::task::spawn_blocking(move || {
                let input = File::open(path.as_path())?;
                let (format, decoded): (_, Box<dyn Read + Send>) =
                    if crate::framed::is_framed(&input)? {
                        (
                            "framed",
                            match scope {
                                ReaderScope::Head => match crate::framed::read_first_frame(&input)?
                                {
                                    Some(head) => Box::new(io::Cursor::new(head)),
                                    None => Box::new(crate::framed::FramedReader::open(input)?),
                                },
                                ReaderScope::Full => {
                                    Box::new(crate::framed::FramedReader::open(input)?)
                                }
                            },
                        )
                    } else {
                        (
                            "zstd",
                            Box::new(super::read_diagnostics::DiagnosticDecoder::new(input)?),
                        )
                    };
                Ok::<_, io::Error>((format, io::BufReader::new(decoded).lines()))
            })
            .await
            .map_err(io::Error::other)
            .inspect_err(|err| metrics.failed("open", ReadFailureSource::TaskJoin, err))??;
            metrics.format = format;
            return Ok(RolloutLineReaderInner::Blocking(Some(reader)));
        }
        metrics.format = "plain";
        let file = tokio::fs::File::open(path).await?;
        Ok(RolloutLineReaderInner::Plain(
            tokio::io::BufReader::new(file).lines(),
        ))
    }
}

#[cfg(test)]
#[path = "compression_tests.rs"]
mod tests;
