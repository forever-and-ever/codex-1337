//! The host, not a renderer's destructors, owns the real terminal's mode lease.

use super::worker_process;
use std::io;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use tokio::io::unix::AsyncFd;

// A worker may die inside synchronized output or while owning either screen's
// input modes. End that lease before painting recovery or yielding to the shell.
pub(super) const RESET_TERMINAL: &[u8] = b"\x1b[?2026l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l\x1b[=0u\x1b[>4;0m\x1b[?1049l\x1b[=0u\x1b[?2004l\x1b[?1004l\x1b[?1007l\x1b[0m\x1b[?25h";

pub(super) struct RealTerminal {
    pub(super) input: AsyncFd<OwnedFd>,
    pub(super) output: AsyncFd<OwnedFd>,
    input_flags: i32,
    output_flags: i32,
}

impl RealTerminal {
    pub(super) fn suspend(&self) -> io::Result<()> {
        crossterm::terminal::disable_raw_mode()?;
        io::stdout().write_all(RESET_TERMINAL)?;
        io::stdout().flush()?;
        // Do not leave the shell's shared open-file descriptions nonblocking.
        // SAFETY: both descriptors remain owned throughout suspension.
        unsafe {
            libc::fcntl(
                self.input.get_ref().as_raw_fd(),
                libc::F_SETFL,
                self.input_flags,
            );
            libc::fcntl(
                self.output.get_ref().as_raw_fd(),
                libc::F_SETFL,
                self.output_flags,
            );
            libc::kill(0, libc::SIGTSTP);
        }
        worker_process::set_nonblocking(self.input.get_ref().as_raw_fd())?;
        worker_process::set_nonblocking(self.output.get_ref().as_raw_fd())?;
        crossterm::terminal::enable_raw_mode()
    }

    pub(super) fn new() -> io::Result<Self> {
        fn duplicate(fd: i32) -> io::Result<OwnedFd> {
            // SAFETY: duplicate a live standard descriptor; the result gets a unique owner.
            let fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
            if fd == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(unsafe { OwnedFd::from_raw_fd(fd) })
            }
        }
        let input = AsyncFd::new(duplicate(/*fd*/ 0)?)?;
        let output = AsyncFd::new(duplicate(/*fd*/ 1)?)?;
        // stdin/stdout may share one open-file description. Read both original
        // flag sets before changing either, or restoration can leave the shell nonblocking.
        // SAFETY: both descriptors are owned and live.
        let input_flags = unsafe { libc::fcntl(input.get_ref().as_raw_fd(), libc::F_GETFL) };
        let output_flags = unsafe { libc::fcntl(output.get_ref().as_raw_fd(), libc::F_GETFL) };
        if input_flags == -1 || output_flags == -1 {
            return Err(io::Error::last_os_error());
        }
        let terminal = Self {
            input,
            output,
            input_flags,
            output_flags,
        };
        worker_process::set_nonblocking(terminal.input.get_ref().as_raw_fd())?;
        worker_process::set_nonblocking(terminal.output.get_ref().as_raw_fd())?;
        crossterm::terminal::enable_raw_mode()?;
        Ok(terminal)
    }
}

impl Drop for RealTerminal {
    fn drop(&mut self) {
        // SAFETY: restore only the descriptor flags changed by this owner.
        unsafe {
            libc::fcntl(
                self.input.get_ref().as_raw_fd(),
                libc::F_SETFL,
                self.input_flags,
            );
            libc::fcntl(
                self.output.get_ref().as_raw_fd(),
                libc::F_SETFL,
                self.output_flags,
            );
        }
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = io::stdout().write_all(RESET_TERMINAL);
        let _ = io::stdout().flush();
    }
}
