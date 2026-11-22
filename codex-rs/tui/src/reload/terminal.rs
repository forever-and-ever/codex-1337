//! The host, not a renderer's destructors, owns the real terminal's mode lease.

use super::worker_process;
use std::io;
use std::io::Write;
use std::os::fd::OwnedFd;
use tokio::io::unix::AsyncFd;

// A worker may die inside synchronized output or while owning either screen's
// input modes. End that lease before painting recovery or yielding to the shell.
pub(super) const RESET_TERMINAL: &[u8] = b"\x1b[?2026l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l\x1b[=0u\x1b[>4;0m\x1b[?1049l\x1b[=0u\x1b[?2004l\x1b[?1004l\x1b[?1007l\x1b[0m\x1b[?25h";

pub(super) struct RealTerminal {
    input: AsyncFd<OwnedFd>,
    output: AsyncFd<OwnedFd>,
    input_flags: rustix::fs::OFlags,
    output_flags: rustix::fs::OFlags,
}

impl RealTerminal {
    pub(super) fn input(&self) -> &AsyncFd<OwnedFd> {
        &self.input
    }

    pub(super) fn output(&self) -> &AsyncFd<OwnedFd> {
        &self.output
    }

    pub(super) fn suspend(&self) -> io::Result<()> {
        crossterm::terminal::disable_raw_mode()?;
        io::stdout().write_all(RESET_TERMINAL)?;
        io::stdout().flush()?;
        rustix::fs::fcntl_setfl(self.input.get_ref(), self.input_flags)?;
        rustix::fs::fcntl_setfl(self.output.get_ref(), self.output_flags)?;
        rustix::process::kill_current_process_group(rustix::process::Signal::TSTP)?;
        worker_process::set_nonblocking(self.input.get_ref())?;
        worker_process::set_nonblocking(self.output.get_ref())?;
        crossterm::terminal::enable_raw_mode()
    }

    pub(super) fn new() -> io::Result<Self> {
        let input = rustix::io::fcntl_dupfd_cloexec(io::stdin(), 3)?;
        let output = rustix::io::fcntl_dupfd_cloexec(io::stdout(), 3)?;
        // SAFETY: The registrations own distinct duplicates and are only shared by reference.
        let (input, output) = unsafe { (AsyncFd::register(input)?, AsyncFd::register(output)?) };
        // stdin/stdout may share one open-file description; capture both before either changes.
        let input_flags = rustix::fs::fcntl_getfl(input.get_ref())?;
        let output_flags = rustix::fs::fcntl_getfl(output.get_ref())?;
        let terminal = Self {
            input,
            output,
            input_flags,
            output_flags,
        };
        worker_process::set_nonblocking(terminal.input.get_ref())?;
        worker_process::set_nonblocking(terminal.output.get_ref())?;
        crossterm::terminal::enable_raw_mode()?;
        Ok(terminal)
    }
}

impl Drop for RealTerminal {
    fn drop(&mut self) {
        let _ = rustix::fs::fcntl_setfl(self.input.get_ref(), self.input_flags);
        let _ = rustix::fs::fcntl_setfl(self.output.get_ref(), self.output_flags);
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = io::stdout().write_all(RESET_TERMINAL);
        let _ = io::stdout().flush();
    }
}
