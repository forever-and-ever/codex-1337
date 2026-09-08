use std::fs;

use anyhow::{Context as _, Result};

/// PID plus kernel start time: a recycled PID is not the same process.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProcessKey {
    pub pid: u32,
    start_ticks: u64,
}

impl ProcessKey {
    pub fn sight(pid: u32) -> Result<Self> {
        Ok(Self {
            pid,
            start_ticks: start_ticks(pid)?,
        })
    }

    pub fn alive(self) -> bool {
        start_ticks(self.pid).is_ok_and(|start| start == self.start_ticks)
    }
}

fn start_ticks(pid: u32) -> Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .with_context(|| format!("read identity of process {pid}"))?;
    stat.rsplit_once(") ")
        .context("malformed /proc process stat")?
        .1
        .split_ascii_whitespace()
        .nth(19)
        .context("process stat omits start time")?
        .parse()
        .context("decode process start time")
}
