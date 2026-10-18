use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use regex::Regex;
use tokio::sync::broadcast;
use tokio::sync::oneshot;

use super::process::OutputBuffers;
use super::process::UnifiedExecProcess;
use crate::CodexThread;

const LINE_LIMIT: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct TerminalWatches {
    watchers: Mutex<WatchState>,
    next: AtomicU64,
}

enum WatchState {
    Open(BTreeMap<u64, Watcher>),
    Closed { at: i64 },
}

impl Default for WatchState {
    fn default() -> Self {
        Self::Open(BTreeMap::new())
    }
}

struct Watcher {
    pattern: Option<Arc<Regex>>,
    line: Line,
    signal: oneshot::Sender<Signal>,
}

enum Line {
    Assembling(Vec<u8>),
    Discarding,
}

enum Signal {
    Matched { at: i64, line: String },
    Closed { at: i64 },
}

pub struct TerminalWatch {
    process: Arc<UnifiedExecProcess>,
    runtime: std::sync::Weak<crate::session::session::Session>,
    watches: Arc<TerminalWatches>,
    id: u64,
    signal: oneshot::Receiver<Signal>,
    command: String,
    retained: String,
    pattern: Option<Arc<Regex>>,
}

pub struct TerminalExitProbe(Arc<UnifiedExecProcess>);

impl TerminalExitProbe {
    #[must_use]
    pub fn has_exited(&self) -> bool {
        self.0.has_exited()
    }
}

pub enum TerminalWatchEvent {
    Matched(String),
    Exited { code: Option<i32>, tail: String },
    Lost,
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalWatchError {
    #[error("session {0} does not exist in this conversation")]
    Missing(i32),
    #[error("session {session_id} has already exited (exit code {code:?}); no alarm was armed")]
    Exited { session_id: i32, code: Option<i32> },
}

// Publication and subscription share the output lock, so arming is an exact output boundary.
pub(super) async fn publish_output<const N: usize>(
    buffer: &tokio::sync::Mutex<OutputBuffers<N>>,
    sender: &broadcast::Sender<Vec<u8>>,
    watches: &TerminalWatches,
    chunk: Vec<u8>,
) {
    let mut buffer = buffer.lock().await;
    buffer.push_chunk(&chunk);
    watches.feed(&chunk);
    let _ = sender.send(chunk);
}

impl TerminalWatches {
    #[expect(
        clippy::expect_used,
        reason = "A poisoned terminal-watch registry cannot be recovered safely"
    )]
    fn lock(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.watchers.lock().expect("terminal watches poisoned")
    }

    fn feed(&self, chunk: &[u8]) {
        let at = timestamp();
        let mut watchers = self.lock();
        let WatchState::Open(watchers) = &mut *watchers else {
            return;
        };
        let mut matched = Vec::new();
        for (id, watcher) in watchers.iter_mut() {
            if let Some(line) = watcher.feed(chunk) {
                matched.push((*id, line));
            }
        }
        for (id, line) in matched {
            if let Some(watcher) = watchers.remove(&id) {
                let _ = watcher.signal.send(Signal::Matched { at, line });
            }
        }
    }

    pub(super) fn close(&self) {
        let at = timestamp();
        let mut state = self.lock();
        if matches!(*state, WatchState::Closed { .. }) {
            return;
        }
        let WatchState::Open(watchers) = std::mem::replace(&mut *state, WatchState::Closed { at })
        else {
            unreachable!("open stream");
        };
        for (_, watcher) in watchers {
            let _ = watcher.signal.send(Signal::Closed { at });
        }
    }
}

impl Watcher {
    // Lines longer than 64 KiB are matched on that prefix, then discarded until the next newline.
    fn feed(&mut self, chunk: &[u8]) -> Option<String> {
        let pattern = self.pattern.as_ref()?;
        for byte in chunk {
            match &mut self.line {
                Line::Discarding => {
                    if *byte == b'\n' {
                        self.line = Line::Assembling(Vec::new());
                    }
                }
                Line::Assembling(line) if *byte == b'\n' || line.len() == LINE_LIMIT => {
                    let text = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line));
                    if pattern.is_match(&text) {
                        return Some(text.chars().take(500).collect());
                    }
                    self.line = if *byte == b'\n' {
                        Line::Assembling(Vec::new())
                    } else {
                        Line::Discarding
                    };
                }
                Line::Assembling(line) => line.push(*byte),
            }
        }
        None
    }
}

impl CodexThread {
    pub async fn watch_background_terminal(
        &self,
        session_id: i32,
        pattern: Option<Regex>,
    ) -> Result<TerminalWatch, TerminalWatchError> {
        let (process, command) = {
            let store = self
                .session
                .services
                .unified_exec_manager
                .process_store
                .lock()
                .await;
            let entry = store
                .processes
                .get(&session_id)
                .ok_or(TerminalWatchError::Missing(session_id))?;
            (
                Arc::clone(&entry.process),
                entry
                    .hook_context
                    .command
                    .chars()
                    .take(512)
                    .collect::<String>(),
            )
        };
        let output = process.output_handles().output_buffer.lock().await;
        if process.has_exited() {
            return Err(TerminalWatchError::Exited {
                session_id,
                code: process.exit_code(),
            });
        }
        let bytes = output.transcript.to_bytes();
        let retained = String::from_utf8_lossy(&bytes).into_owned();
        let partial = bytes
            .rsplit(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        let line = if partial.len() <= LINE_LIMIT {
            Line::Assembling(partial.to_vec())
        } else {
            Line::Discarding
        };
        let pattern = pattern.map(Arc::new);
        let watches = Arc::clone(&process.output_handles().terminal_watches);
        let id = watches.next.fetch_add(1, Ordering::Relaxed);
        let (sender, signal) = oneshot::channel();
        {
            let mut state = watches.lock();
            match &mut *state {
                WatchState::Open(watchers) => {
                    watchers.insert(
                        id,
                        Watcher {
                            pattern: pattern.clone(),
                            line,
                            signal: sender,
                        },
                    );
                }
                WatchState::Closed { at } => {
                    let _ = sender.send(Signal::Closed { at: *at });
                }
            }
        }
        drop(output);
        Ok(TerminalWatch {
            process,
            runtime: Arc::downgrade(&self.session),
            watches,
            id,
            signal,
            command,
            retained,
            pattern,
        })
    }
}

impl Drop for TerminalWatch {
    fn drop(&mut self) {
        if let WatchState::Open(watchers) = &mut *self.watches.lock() {
            watchers.remove(&self.id);
        }
    }
}

impl TerminalWatch {
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }
    #[must_use]
    pub fn pattern(&self) -> Option<&str> {
        self.pattern.as_ref().map(|pattern| pattern.as_str())
    }
    #[must_use]
    pub fn retained_output(&self) -> &str {
        &self.retained
    }

    /// Inspects the watched process's exit state while `wait` owns the watch.
    #[must_use]
    pub fn exit_probe(&self) -> TerminalExitProbe {
        TerminalExitProbe(Arc::clone(&self.process))
    }

    pub async fn wait(mut self) -> (i64, TerminalWatchEvent) {
        match (&mut self.signal).await {
            Ok(Signal::Matched { at, line }) => (at, TerminalWatchEvent::Matched(line)),
            Ok(Signal::Closed { at }) => {
                self.process.cancellation_token().cancelled().await;
                let exited_at = timestamp().max(at);
                if self.runtime_closed().await {
                    return (exited_at, TerminalWatchEvent::Lost);
                }
                let output = self.process.output_handles().output_buffer.lock().await;
                let text = String::from_utf8_lossy(&output.transcript.to_bytes()).into_owned();
                let mut lines = text.lines().rev().take(40).collect::<Vec<_>>();
                lines.reverse();
                let tail = lines.join("\n");
                let start = tail
                    .char_indices()
                    .rev()
                    .nth(3999)
                    .map_or(0, |(offset, _)| offset);
                let tail = tail[start..].to_owned();
                (
                    exited_at,
                    TerminalWatchEvent::Exited {
                        code: self.process.exit_code(),
                        tail,
                    },
                )
            }
            Err(_) => {
                unreachable!("a registered terminal watch always publishes its terminal signal")
            }
        }
    }

    async fn runtime_closed(&self) -> bool {
        match self.runtime.upgrade() {
            Some(runtime) => runtime.terminal_runtime_closed().await,
            None => true,
        }
    }
}

fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
