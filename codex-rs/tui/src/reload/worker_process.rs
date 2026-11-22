//! PTY isolation gives the host exclusive authority over the real terminal.
//! Process creation and reaping never run on the reload deadline's reactor.

use std::ffi::OsString;
use std::io;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio::io::AsyncReadExt;
use tokio::io::unix::AsyncFd;

const WORKER_LIMIT: usize = 3;

pub(super) struct Worker {
    terminal: AsyncFd<OwnedFd>,
    pub(super) executable: PathBuf,
    child: Option<Child>,
    slot: Option<Slot>,
    reaper: Reaper,
}

pub(super) struct Launch {
    pub(super) executable: PathBuf,
    pub(super) args: Vec<OsString>,
    pub(super) directory: PathBuf,
    pub(super) generation: u64,
    pub(super) size: (u16, u16),
    pub(super) slot: Slot,
    pub(super) reaper: Reaper,
}

pub(super) struct Slot(Arc<AtomicUsize>);

/// Fixed capacity matches the launch slots. Neither shutdown nor thread creation
/// may block the supervisor when it retires a kernel-stalled worker.
#[derive(Clone)]
pub(super) struct Reaper(std::sync::mpsc::SyncSender<(Child, Option<Slot>)>);

impl Reaper {
    pub(super) fn new() -> io::Result<Self> {
        let (sender, receiver) =
            std::sync::mpsc::sync_channel::<(Child, Option<Slot>)>(WORKER_LIMIT);
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        for _ in 0..WORKER_LIMIT {
            let receiver = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name("ui-reaper".into())
                .spawn(move || {
                    loop {
                        let job = receiver
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .recv();
                        let Ok((mut child, slot)) = job else { break };
                        // The receiver lock has been released; a stalled wait uses only its own slot.
                        let _ = child.wait();
                        drop(slot);
                    }
                })?;
        }
        Ok(Self(sender))
    }
}

impl Slot {
    pub(super) fn reserve(count: &Arc<AtomicUsize>) -> Option<Self> {
        count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < WORKER_LIMIT).then_some(count + 1)
            })
            .ok()
            .map(|_| Self(Arc::clone(count)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

impl Launch {
    /// Never run an incompatible binary with the original prompt/fork arguments.
    /// This probe has its own ceiling and cannot delay the supervisor's deadline.
    pub(super) async fn negotiate(&mut self) -> io::Result<()> {
        let check = async {
            self.executable = tokio::fs::canonicalize(&self.executable).await?;
            let mut child = tokio::process::Command::new(&self.executable)
                .args(["debug", "tui-reload-probe"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let expected = format!("{}\n", super::retained::PROTOCOL);
            let mut bytes = vec![0; expected.len()];
            child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("Missing UI probe output pipe"))?
                .read_exact(&mut bytes)
                .await?;
            if bytes != expected.as_bytes() || !child.wait().await?.success() {
                return Err(io::Error::other(
                    "Installed UI does not support this reload protocol",
                ));
            }
            Ok(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 1), check)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UI protocol probe timed out"))?
    }

    pub(super) fn spawn(self) -> io::Result<Worker> {
        let executable = std::fs::canonicalize(self.executable)?;
        let mut master = -1;
        let mut slave = -1;
        let size = dimensions(self.size);
        // SAFETY: openpty initializes both descriptors on success; each acquires one owner.
        if unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &raw const size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: these are distinct, newly allocated descriptors from openpty.
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        // SAFETY: openpty returned a distinct descriptor whose custody is transferred here.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        for fd in [&master, &slave] {
            rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC)?;
        }
        set_nonblocking(&master)?;
        // SAFETY: The registration owns the PTY master; callers can only borrow it.
        let terminal = unsafe { AsyncFd::register(master)? };
        let mut command = Command::new(&executable);
        command
            .args(self.args)
            .env("CODEX_UI_HOST", self.directory)
            .env("CODEX_UI_GENERATION", self.generation.to_string())
            .env("CODEX_UI_PARENT", std::process::id().to_string())
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe syscalls execute between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(Worker {
            terminal,
            executable,
            child: Some(command.spawn()?),
            slot: Some(self.slot),
            reaper: self.reaper,
        })
    }
}

impl Worker {
    pub(super) fn terminal(&self) -> &AsyncFd<OwnedFd> {
        &self.terminal
    }

    pub(super) fn resize(&self, size: (u16, u16)) -> io::Result<()> {
        rustix::termios::tcsetwinsize(
            self.terminal.get_ref(),
            rustix::termios::Winsize {
                ws_col: size.0,
                ws_row: size.1,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )?;
        Ok(())
    }

    pub(super) fn exit_code(&mut self) -> io::Result<Option<i32>> {
        self.child.as_mut().map_or(Ok(Some(1)), |child| {
            child
                .try_wait()
                .map(|status| status.map(|status| status.code().unwrap_or(1)))
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let slot = self.slot.take();
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            // Each worker has its own session/process group. Never signal another terminal.
            let pid = rustix::process::Pid::from_child(&child);
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            if let Err(
                std::sync::mpsc::TrySendError::Full(job)
                | std::sync::mpsc::TrySendError::Disconnected(job),
            ) = self.reaper.0.try_send((child, slot))
            {
                // A dead reaper must not free a possibly unreaped slot or force
                // synchronous waiting. Keep the launch bound even in this failure.
                std::mem::forget(job);
            }
        }
    }
}

fn dimensions((columns, rows): (u16, u16)) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

pub(super) fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(fd)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

pub(super) fn read(fd: &OwnedFd, bytes: &mut [u8]) -> io::Result<usize> {
    Ok(rustix::io::read(fd, bytes)?)
}

pub(super) fn write(fd: &OwnedFd, bytes: &[u8]) -> io::Result<usize> {
    Ok(rustix::io::write(fd, bytes)?)
}

#[cfg(test)]
#[path = "worker_process_tests.rs"]
mod tests;
