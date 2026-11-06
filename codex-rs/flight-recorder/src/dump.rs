//! The recorder's only paths to disk: an on-demand `SIGUSR1` dump and the panic hook.
//!
//! A dump is named `{process}.{cause}.{pid}.{unix_ms}.log`. Each process kind and cause keeps
//! its own newest [`DUMP_RETENTION`] files, so an on-demand dump, or another process's dump,
//! never deletes panic evidence.

use std::cmp::Reverse;
use std::fs;
use std::fs::File;
use std::io;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use crate::FlightRecorder;
use crate::RecordFilter;
use crate::layer::unix_ms;
use crate::render::render_record;

/// Dumps kept per process kind and cause.
const DUMP_RETENTION: usize = 4;
/// The panic hook's only buffer: records stream from the ring through it.
const PANIC_BUFFER_BYTES: usize = 64 << 10;
/// A panic while another thread briefly holds the ring waits this long, never indefinitely.
const PANIC_LOCK_ATTEMPTS: usize = 50;
const PANIC_LOCK_BACKOFF: Duration = Duration::from_millis(1);

/// The kind of process whose ring a dump holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Process {
    AppServer,
    Tui,
}

impl Process {
    fn as_str(self) -> &'static str {
        match self {
            Self::AppServer => "app-server",
            Self::Tui => "tui",
        }
    }
}

#[derive(Clone, Copy)]
enum Cause {
    Signal,
    Panic,
}

impl Cause {
    fn as_str(self) -> &'static str {
        match self {
            Self::Signal => "signal",
            Self::Panic => "panic",
        }
    }
}

pub fn dump_dir(codex_home: &Path) -> PathBuf {
    codex_home.join("log").join("flight-recorder")
}

impl FlightRecorder {
    /// Writes the whole ring to a new file under `dir`, then prunes this process kind's
    /// older on-demand dumps.
    pub fn dump(&self, dir: &Path, process: Process) -> io::Result<PathBuf> {
        let records = self.excerpt(&RecordFilter::default()).records;
        fs::create_dir_all(dir)?;
        let path = dir.join(dump_name(process, Cause::Signal));
        let mut out = BufWriter::new(File::create_new(&path)?);
        for record in &records {
            render_record(record, &mut out)?;
        }
        out.flush()?;
        prune(dir, process, Cause::Signal)?;
        Ok(path)
    }
}

fn dump_name(process: Process, cause: Cause) -> String {
    format!(
        "{}.{}.{}.{}.log",
        process.as_str(),
        cause.as_str(),
        std::process::id(),
        unix_ms(SystemTime::now())
    )
}

/// Deletes all but the newest [`DUMP_RETENTION`] dumps of one process kind and cause.
fn prune(dir: &Path, process: Process, cause: Cause) -> io::Result<()> {
    let prefix = format!("{}.{}.", process.as_str(), cause.as_str());
    let mut dumps = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (_pid, unix_ms) = name
                .strip_prefix(&prefix)?
                .strip_suffix(".log")?
                .split_once('.')?;
            Some((unix_ms.parse::<i64>().ok()?, entry.path()))
        })
        .collect::<Vec<_>>();
    dumps.sort_unstable_by_key(|dump| Reverse(dump.0));
    for (_, path) in dumps.into_iter().skip(DUMP_RETENTION) {
        // Another process of the same kind may be pruning the same files.
        match fs::remove_file(path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
    }
    Ok(())
}

/// Chains a hook that writes the ring to `dir` before the previous hook runs.
///
/// The hook itself never prunes; older panic dumps of this process kind are pruned here, at
/// startup.
pub fn install_panic_hook(recorder: &'static FlightRecorder, dir: PathBuf, process: Process) {
    if dir.is_dir()
        && let Err(err) = prune(&dir, process, Cause::Panic)
    {
        tracing::warn!("flight recorder panic dump pruning failed: {err}");
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        dump_on_panic(recorder, &dir, process, info);
        previous(info);
    }));
}

fn dump_on_panic(
    recorder: &FlightRecorder,
    dir: &Path,
    process: Process,
    info: &std::panic::PanicHookInfo<'_>,
) {
    // A panic inside the ring's own critical section leaves the lock held by this thread;
    // blocking on it would turn the panic into a hang.
    let mut ring = None;
    for _ in 0..PANIC_LOCK_ATTEMPTS {
        ring = recorder.try_lock();
        if ring.is_some() {
            break;
        }
        std::thread::sleep(PANIC_LOCK_BACKOFF);
    }
    let Some(ring) = ring else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(file) = File::create_new(dir.join(dump_name(process, Cause::Panic))) else {
        return;
    };
    let mut out = BufWriter::with_capacity(PANIC_BUFFER_BYTES, file);
    let _ = writeln!(out, "panic: {info}");
    ring.for_each_in_order(|record| {
        let _ = render_record(record, &mut out);
    });
    let _ = out.flush();
}

/// Dumps the ring to `dir` each time the process receives `SIGUSR1`.
///
/// The dump runs on its own OS thread: it is for a wedged process, including one whose async
/// runtime is blocked.
#[cfg(unix)]
pub fn spawn_signal_dump(
    recorder: &'static FlightRecorder,
    dir: PathBuf,
    process: Process,
) -> io::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGUSR1])?;
    std::thread::Builder::new()
        .name("flight-recorder-dump".to_owned())
        .spawn(move || {
            for _ in signals.forever() {
                match recorder.dump(&dir, process) {
                    Ok(path) => tracing::info!(path = %path.display(), "flight recorder dumped"),
                    Err(err) => tracing::warn!("flight recorder dump failed: {err}"),
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
#[path = "dump_tests.rs"]
mod tests;
