//! The open file a rollout writer appends lines to: framed `.jsonl.zst`, or plain JSONL until
//! `codex rollouts compress` converts it.

use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Instant;

use tokio::io::AsyncWriteExt;

use crate::framed::FramedReader;
use crate::framed::FramedWriter;
use crate::ordinal::RolloutOrdinalState;
use crate::ordinal::ordinal_state_for_rollout;

pub(crate) enum RolloutSink {
    Plain(tokio::fs::File),
    /// Compression and positional writes run on the blocking pool, which the writer moves to for
    /// each operation; the mutex is never contended.
    Framed(Arc<Mutex<FramedWriter>>),
}

impl RolloutSink {
    /// Opens `path`, already resolved for append, creating it if missing. A framed rollout's
    /// open frame is sealed first. Blocking; the caller holds the writer lock.
    pub(crate) fn create(path: &Path) -> io::Result<Self> {
        Self::open(path, /*create*/ true, /*read_ordinals*/ false).map(|(sink, _)| sink)
    }

    /// Opens an existing rollout as [`Self::create`] does and reads where its record ordinals
    /// continue.
    pub(crate) fn resume(path: &Path) -> io::Result<(Self, RolloutOrdinalState)> {
        let (sink, ordinal_state) =
            Self::open(path, /*create*/ false, /*read_ordinals*/ true)?;
        Ok((sink, ordinal_state.unwrap_or(RolloutOrdinalState::Legacy)))
    }

    fn open(
        path: &Path,
        create: bool,
        read_ordinals: bool,
    ) -> io::Result<(Self, Option<RolloutOrdinalState>)> {
        if create && let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if crate::compression::is_compressed_rollout_path(path) {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(create)
                .truncate(false)
                .open(path)?;
            let writer = FramedWriter::open(path, file)?;
            let ordinal_state = if read_ordinals {
                let mut reader = FramedReader::open(writer.file().try_clone()?)?;
                Some(ordinal_state_for_rollout(&mut reader, path)?)
            } else {
                None
            };
            return Ok((Self::Framed(Arc::new(Mutex::new(writer))), ordinal_state));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(create)
            .open(path)?;
        terminate_torn_line(&mut file)?;
        let ordinal_state = if read_ordinals {
            Some(ordinal_state_for_rollout(&mut file, path)?)
        } else {
            None
        };
        Ok((Self::Plain(tokio::fs::File::from_std(file)), ordinal_state))
    }

    /// Appends one newline-terminated line. A framed append is durable against a process crash
    /// when this returns.
    pub(crate) async fn append_line(&mut self, line: Vec<u8>) -> io::Result<Vec<u8>> {
        match self {
            Self::Plain(file) => {
                file.write_all(&line).await?;
                file.flush().await?;
                Ok(line)
            }
            Self::Framed(writer) => {
                off_runtime(writer, move |writer| {
                    writer.append(&line, /*flush*/ true)?;
                    Ok(line)
                })
                .await
            }
        }
    }

    pub(crate) async fn flushed_logical_len(&self) -> io::Result<Option<u64>> {
        match self {
            Self::Plain(file) => Ok(Some(file.metadata().await?.len())),
            Self::Framed(writer) => {
                off_runtime(writer, |writer| writer.flushed_logical_len()).await
            }
        }
    }

    pub(crate) async fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(file) => file.flush().await,
            Self::Framed(_) => Ok(()),
        }
    }

    pub(crate) async fn sync_data(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(file) => file.sync_data().await,
            Self::Framed(writer) => off_runtime(writer, |writer| writer.file().sync_data()).await,
        }
    }

    /// When the open frame should close because no line arrived.
    pub(crate) fn idle_deadline(&self) -> Option<Instant> {
        match self {
            Self::Plain(_) => None,
            Self::Framed(writer) => lock(writer).ok()?.idle_deadline(),
        }
    }

    /// Ends the open frame so the next writer need not seal it.
    pub(crate) async fn close_frame(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(_) => Ok(()),
            Self::Framed(writer) => off_runtime(writer, FramedWriter::close_frame).await,
        }
    }
}

/// Runs one framed-writer operation on the blocking pool.
async fn off_runtime<T: Send + 'static>(
    writer: &Arc<Mutex<FramedWriter>>,
    operation: impl FnOnce(&mut FramedWriter) -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let writer = Arc::clone(writer);
    tokio::task::spawn_blocking(move || operation(&mut *lock(&writer)?))
        .await
        .map_err(io::Error::other)?
}

fn lock(writer: &Mutex<FramedWriter>) -> io::Result<MutexGuard<'_, FramedWriter>> {
    writer
        .lock()
        .map_err(|_| io::Error::other("a framed rollout writer panicked"))
}

/// A plain rollout torn mid-line gets a newline, so the next record starts on its own line.
fn terminate_torn_line(file: &mut File) -> io::Result<()> {
    if file.metadata()?.len() == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut final_byte = [0];
    file.read_exact(&mut final_byte)?;
    if final_byte[0] != b'\n' {
        file.write_all(b"\n")?;
        file.flush()?;
    }
    Ok(())
}
