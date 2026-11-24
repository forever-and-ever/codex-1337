#![allow(clippy::module_inception)]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::broadcast;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use codex_exec_server::ExecOutputStream;
use codex_exec_server::ExecProcess;
use codex_exec_server::ExecProcessEvent;
use codex_exec_server::ProcessSignal as ExecServerProcessSignal;
use codex_exec_server::ReadResponse as ExecReadResponse;
use codex_exec_server::StartedExecProcess;
use codex_exec_server::WriteStatus;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::exec_output::StreamOutput;
use codex_protocol::protocol::TruncationPolicy;
use codex_sandboxing::SandboxType;
use codex_sandboxing::is_likely_sandbox_denied;
use codex_sandboxing::record_filesystem_sandbox_violation;
use codex_utils_head_tail_buffer::HeadTailBuffer;
use codex_utils_output_truncation::formatted_truncate_text;
use codex_utils_pty::ExecCommandSession;
use codex_utils_pty::ProcessSignal as PtyProcessSignal;
use codex_utils_pty::SpawnedPty;

use super::UNIFIED_EXEC_OUTPUT_MAX_BYTES;
use super::UNIFIED_EXEC_OUTPUT_MAX_TOKENS;
use super::UnifiedExecError;
use super::process_state::ProcessState;
use crate::shell_snapshot::ShellSnapshotFile;

const EARLY_EXIT_GRACE_PERIOD: Duration = Duration::from_millis(150);
const REMOTE_OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Output pending model polling and the retained completion transcript.
/// Append both under the same lock so cancellation cannot split an update.
#[derive(Default)]
pub(crate) struct OutputBuffers<const MAX_BYTES: usize = UNIFIED_EXEC_OUTPUT_MAX_BYTES> {
    pub(super) pending: HeadTailBuffer<MAX_BYTES>,
    pub(super) transcript: HeadTailBuffer<MAX_BYTES>,
}

impl<const MAX_BYTES: usize> OutputBuffers<MAX_BYTES> {
    pub(super) fn push_chunk(&mut self, _stream: ExecOutputStream, chunk: &[u8]) {
        self.pending.push_chunk(chunk);
        self.transcript.push_chunk(chunk);
    }
}

/// Shared output state exposed to polling and streaming consumers.
#[derive(Clone)]
pub(crate) struct OutputHandles<const MAX_BYTES: usize = UNIFIED_EXEC_OUTPUT_MAX_BYTES> {
    pub(crate) output_buffer: Arc<Mutex<OutputBuffers<MAX_BYTES>>>,
    pub(crate) output_notify: Arc<Notify>,
    pub(crate) output_closed: Arc<super::runtime_hold::OutputTermination>,
    pub(crate) output_closed_notify: Arc<Notify>,
    pub(crate) cancellation_token: CancellationToken,
    pub(super) terminal_watches: Arc<super::terminal_watch::TerminalWatches>,
    pub(super) drain_policy: OutputDrainPolicy,
    /// The initial exec call returned or was cancelled, so background draining may begin.
    pub(crate) collection_finished: CancellationToken,
}

/// Ends output when its task ends. Built before the task is spawned, so an abort that drops the
/// task unpolled still releases waiters on output closure.
struct OutputTaskGuard {
    output_closed: Arc<super::runtime_hold::OutputTermination>,
    output_closed_notify: Arc<Notify>,
    terminal_watches: Arc<super::terminal_watch::TerminalWatches>,
}

impl Drop for OutputTaskGuard {
    fn drop(&mut self) {
        self.terminal_watches.close();
        self.output_closed.interrupt();
        self.output_closed_notify.notify_waiters();
    }
}

/// Controls whether process exit can start the collectors' bounded output drain.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputDrainPolicy {
    /// Match direct local execution when descendants retain inherited output pipes.
    BoundedAfterExit,
    /// Remote foreground output may still be buffered or in transit after exit.
    WaitForOutputClosure,
}

/// Transport-specific process handle used by unified exec.
enum ProcessHandle {
    Local(Box<ExecCommandSession>),
    ExecServer(Arc<dyn ExecProcess>),
}

/// Unified wrapper over directly spawned PTY sessions and exec-server-backed
/// processes.
pub(crate) struct UnifiedExecProcess {
    process_handle: ProcessHandle,
    output_tx: broadcast::Sender<Vec<u8>>,
    output: OutputHandles,
    output_drained: Arc<super::runtime_hold::OutputDrain>,
    interaction_lock: Arc<Mutex<()>>,
    state_tx: watch::Sender<ProcessState>,
    state_rx: watch::Receiver<ProcessState>,
    output_task: Option<JoinHandle<()>>,
    sandbox_type: Option<SandboxType>,
    timed_out: AtomicBool,
    /// Explicit termination overrides retention of remote output after exit.
    termination: Arc<termination::Termination>,
    // The shell may still need to replay this file after process startup returns.
    pub(crate) _shell_snapshot: std::sync::Mutex<Option<Arc<ShellSnapshotFile>>>,
}

impl std::fmt::Debug for UnifiedExecProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnifiedExecProcess")
            .field("has_exited", &self.has_exited())
            .field("exit_code", &self.exit_code())
            .field("sandbox_type", &self.sandbox_type)
            .finish_non_exhaustive()
    }
}

impl UnifiedExecProcess {
    fn new(
        process_handle: ProcessHandle,
        sandbox_type: Option<SandboxType>,
        drain_policy: OutputDrainPolicy,
    ) -> Self {
        let output = OutputHandles {
            terminal_watches: Arc::default(),
            output_buffer: Arc::new(Mutex::new(OutputBuffers::default())),
            output_notify: Arc::new(Notify::new()),
            output_closed: Arc::default(),
            output_closed_notify: Arc::new(Notify::new()),
            cancellation_token: CancellationToken::new(),
            drain_policy,
            collection_finished: CancellationToken::new(),
        };
        let output_drained = Arc::default();
        let (output_tx, _) = broadcast::channel(64);
        let (state_tx, state_rx) = watch::channel(ProcessState::default());

        Self {
            process_handle,
            output_tx,
            output,
            output_drained,
            interaction_lock: Arc::new(Mutex::new(())),
            state_tx,
            state_rx,
            output_task: None,
            sandbox_type,
            timed_out: AtomicBool::new(false),
            termination: Arc::default(),
            _shell_snapshot: std::sync::Mutex::new(None),
        }
    }

    pub(super) async fn write(&self, data: &[u8]) -> Result<(), UnifiedExecError> {
        match &self.process_handle {
            ProcessHandle::Local(process_handle) => process_handle
                .writer_sender()
                .send(data.to_vec())
                .await
                .map_err(|_| UnifiedExecError::WriteToStdin),
            ProcessHandle::ExecServer(process_handle) => {
                match process_handle.write(data.to_vec()).await {
                    Ok(response) => match response.status {
                        WriteStatus::Accepted => Ok(()),
                        WriteStatus::UnknownProcess | WriteStatus::StdinClosed => {
                            let state = self.state_rx.borrow().clone();
                            let _ = self.state_tx.send_replace(state.exited(state.exit_code));
                            self.output.cancellation_token.cancel();
                            Err(UnifiedExecError::WriteToStdin)
                        }
                        WriteStatus::Starting => Err(UnifiedExecError::WriteToStdin),
                    },
                    Err(err) => Err(UnifiedExecError::process_failed(err.to_string())),
                }
            }
        }
    }

    pub(super) fn output_handles(&self) -> &OutputHandles {
        &self.output
    }

    pub(super) fn output_receiver(&self) -> tokio::sync::broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    pub(super) fn cancellation_token(&self) -> CancellationToken {
        self.output.cancellation_token.clone()
    }

    pub(super) fn output_drained_notify(&self) -> Arc<super::runtime_hold::OutputDrain> {
        Arc::clone(&self.output_drained)
    }

    pub(super) fn interaction_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.interaction_lock)
    }

    pub(super) fn state_updates(&self) -> watch::Receiver<super::process_state::ProcessState> {
        self.state_rx.clone()
    }

    pub(super) fn termination_requested(&self) -> bool {
        self.termination.requested()
    }

    pub(super) fn has_exited(&self) -> bool {
        let state = self.state_rx.borrow().clone();
        match &self.process_handle {
            ProcessHandle::Local(process_handle) => state.has_exited || process_handle.has_exited(),
            ProcessHandle::ExecServer(_) => state.has_exited,
        }
    }

    pub(super) fn backend_has_exited(&self) -> bool {
        termination::backend_has_exited(self)
    }

    pub(super) fn exit_code(&self) -> Option<i32> {
        if self.timed_out() {
            return Some(124);
        }
        let state = self.state_rx.borrow().clone();
        match &self.process_handle {
            ProcessHandle::Local(process_handle) => {
                state.exit_code.or_else(|| process_handle.exit_code())
            }
            ProcessHandle::ExecServer(_) => state.exit_code,
        }
    }

    pub(super) fn mark_timed_out(&self) {
        self.timed_out.store(true, Ordering::Release);
    }

    pub(super) fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::Acquire)
    }

    fn finish_termination(&self) {
        self.output.cancellation_token.cancel();
        if let Some(output_task) = &self.output_task {
            output_task.abort();
        }
    }

    pub(super) fn terminate(&self) {
        termination::terminate(self);
    }

    pub(super) async fn terminate_confirmed(&self) -> Result<(), UnifiedExecError> {
        termination::confirmed(self).await
    }

    pub(super) async fn interrupt(&self) -> Result<(), UnifiedExecError> {
        match &self.process_handle {
            ProcessHandle::Local(process_handle) => process_handle
                .signal(PtyProcessSignal::Interrupt)
                .map_err(|err| UnifiedExecError::process_failed(err.to_string())),
            ProcessHandle::ExecServer(process_handle) => process_handle
                .signal(ExecServerProcessSignal::Interrupt)
                .await
                .map_err(|err| UnifiedExecError::process_failed(err.to_string())),
        }
    }

    pub(super) fn fail_and_terminate(&self, message: String) {
        let state = self.state_rx.borrow().clone();
        if state.failure_message.is_none() {
            let _ = self.state_tx.send_replace(state.failed(message));
        }
        self.terminate();
    }

    async fn snapshot_output(&self) -> Vec<u8> {
        let guard = self.output.output_buffer.lock().await;
        guard.pending.to_bytes()
    }

    pub(crate) fn sandbox_type(&self) -> Option<SandboxType> {
        self.sandbox_type
    }

    pub(super) fn failure_message(&self) -> Option<String> {
        self.state_rx.borrow().failure_message.clone()
    }

    pub(super) async fn check_for_sandbox_denial(&self) -> Result<(), UnifiedExecError> {
        // A closed output stream cannot provide any more denial context.
        if !self.output.output_closed.load(Ordering::Acquire) {
            let closed = self.output.output_closed_notify.notified();
            tokio::pin!(closed);
            closed.as_mut().enable();
            // Recheck after registering so closure cannot be lost before we wait.
            if !self.output.output_closed.load(Ordering::Acquire) {
                let _ = tokio::time::timeout(Duration::from_millis(20), async {
                    tokio::select! {
                        _ = self.output.output_notify.notified() => {}
                        _ = &mut closed => {}
                    }
                })
                .await;
            }
        }

        let aggregated = self.snapshot_output().await;
        let aggregated_text = String::from_utf8_lossy(&aggregated);
        let result = self
            .check_for_sandbox_denial_with_text(aggregated_text.as_ref())
            .await;
        super::runtime_hold::finish_startup(self, result)
    }

    pub(super) async fn check_for_sandbox_denial_with_text(
        &self,
        text: &str,
    ) -> Result<(), UnifiedExecError> {
        let executor_reported_denial = self.state_rx.borrow().sandbox_denied;
        let sandbox_type = self.sandbox_type().unwrap_or(SandboxType::None);
        if !self.has_exited() || (!executor_reported_denial && sandbox_type == SandboxType::None) {
            return Ok(());
        }

        let exit_code = self.exit_code().unwrap_or(-1);
        let exec_output = ExecToolCallOutput {
            exit_code,
            stderr: StreamOutput::new(text.to_string()),
            aggregated_output: StreamOutput::new(text.to_string()),
            ..Default::default()
        };
        let likely_sandbox_denial = is_likely_sandbox_denied(sandbox_type, &exec_output);
        if likely_sandbox_denial {
            record_filesystem_sandbox_violation(sandbox_type, &exec_output);
        }
        if executor_reported_denial || likely_sandbox_denial {
            let snippet = formatted_truncate_text(
                text,
                TruncationPolicy::Tokens(UNIFIED_EXEC_OUTPUT_MAX_TOKENS),
            );
            let message = if snippet.is_empty() {
                format!("Process exited with code {exit_code}")
            } else {
                snippet
            };
            return Err(UnifiedExecError::sandbox_denied(message, exec_output));
        }
        Ok(())
    }

    pub(super) async fn from_spawned(
        spawned: SpawnedPty,
        sandbox_type: SandboxType,
        on_started: impl FnOnce(Arc<Self>),
    ) -> Result<Arc<Self>, UnifiedExecError> {
        let SpawnedPty {
            session: process_handle,
            stdout_rx,
            stderr_rx,
            mut exit_rx,
        } = spawned;
        let mut managed = Self::new(
            ProcessHandle::Local(Box::new(process_handle)),
            Some(sandbox_type),
            OutputDrainPolicy::BoundedAfterExit,
        );
        managed.output_task = Some(Self::spawn_local_output_task(
            stdout_rx,
            stderr_rx,
            managed.output_handles().clone(),
            managed.output_tx.clone(),
        ));

        let managed = Arc::new(managed);
        on_started(Arc::clone(&managed));
        match exit_rx.try_recv() {
            Ok(exit_code) => {
                managed.signal_exit(Some(exit_code));
                managed.check_for_sandbox_denial().await?;
                return Ok(managed);
            }
            Err(TryRecvError::Closed) => {
                managed.signal_exit(/*exit_code*/ None);
                managed.check_for_sandbox_denial().await?;
                return Ok(managed);
            }
            Err(TryRecvError::Empty) => {}
        }

        if let Ok(exit_result) = tokio::time::timeout(EARLY_EXIT_GRACE_PERIOD, &mut exit_rx).await {
            managed.signal_exit(exit_result.ok());
            managed.check_for_sandbox_denial().await?;
            return Ok(managed);
        }

        tokio::spawn({
            let state_tx = managed.state_tx.clone();
            let cancellation_token = managed.output.cancellation_token.clone();
            async move {
                let exit_code = exit_rx.await.ok();
                let state = state_tx.borrow().clone();
                let _ = state_tx.send_replace(state.exited(exit_code));
                cancellation_token.cancel();
            }
        });

        Ok(managed)
    }

    pub(super) async fn from_exec_server_started(
        started: StartedExecProcess,
        output_drain_policy: OutputDrainPolicy,
        on_started: impl FnOnce(Arc<Self>),
    ) -> Result<Arc<Self>, UnifiedExecError> {
        let process_handle = ProcessHandle::ExecServer(Arc::clone(&started.process));
        // Older peers do not report this field. In that case, skip local
        // classification rather than attributing a violation to a guessed backend.
        let sandbox_type = started.sandbox_type;
        let mut managed = Self::new(process_handle, sandbox_type, output_drain_policy);
        let output_handles = managed.output_handles().clone();
        managed.output_task = Some(Self::spawn_exec_server_output_task(
            started,
            output_drain_policy,
            output_handles,
            managed.output_tx.clone(),
            managed.state_tx.clone(),
            managed.interaction_lock(),
            Arc::clone(&managed.termination),
        ));

        let managed = Arc::new(managed);
        on_started(Arc::clone(&managed));
        let mut state_rx = managed.state_rx.clone();
        if tokio::time::timeout(EARLY_EXIT_GRACE_PERIOD, async {
            loop {
                let state = state_rx.borrow().clone();
                if state.has_exited || state.failure_message.is_some() {
                    break;
                }
                if state_rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
        .is_ok()
        {
            managed.check_for_sandbox_denial().await?;
        }

        Ok(managed)
    }

    fn spawn_exec_server_output_task(
        started: StartedExecProcess,
        output_drain_policy: OutputDrainPolicy,
        output_handles: OutputHandles,
        output_tx: broadcast::Sender<Vec<u8>>,
        state_tx: watch::Sender<ProcessState>,
        interaction_lock: Arc<Mutex<()>>,
        termination: Arc<termination::Termination>,
    ) -> JoinHandle<()> {
        let OutputHandles {
            terminal_watches,
            output_buffer,
            output_notify,
            output_closed,
            output_closed_notify,
            cancellation_token,
            collection_finished,
            ..
        } = output_handles;
        let process = started.process;
        let mut events = process.subscribe_events();
        let mut state_rx = state_tx.subscribe();
        let output_task_guard = OutputTaskGuard {
            terminal_watches: Arc::clone(&terminal_watches),
            output_closed: Arc::clone(&output_closed),
            output_closed_notify: Arc::clone(&output_closed_notify),
        };
        tokio::spawn(async move {
            let _output_task_guard = output_task_guard;
            let drain = async {
                collection_finished.cancelled().await;
                let _ = state_rx.wait_for(|state| state.has_exited).await;
                tokio::time::sleep(REMOTE_OUTPUT_DRAIN_GRACE).await;
                // An active poll keeps its own deadline, including transport recovery.
                interaction_lock.lock_owned().await
            };
            tokio::pin!(drain);
            let mut final_drain = false;
            let mut last_seq: u64 = 0;
            loop {
                let received = tokio::select! {
                    received = events.recv() => Some(received),
                    guard = &mut drain, if output_drain_policy == OutputDrainPolicy::WaitForOutputClosure
                        && !final_drain => {
                        // Later polls must remain able to collect output while this read recovers.
                        drop(guard);
                        final_drain = true;
                        None
                    }
                };
                let event = match received {
                    Some(Ok(event)) => Some(event),
                    Some(Err(broadcast::error::RecvError::Lagged(_))) | None => None,
                    Some(Err(broadcast::error::RecvError::Closed)) => {
                        let state = state_tx.borrow().clone();
                        let _ = state_tx.send_replace(
                            state.failed("exec-server process event stream closed".to_string()),
                        );
                        output_closed.fail();
                        output_closed_notify.notify_waiters();
                        cancellation_token.cancel();
                        break;
                    }
                };
                let event_seq = event.as_ref().and_then(|event| match event {
                    ExecProcessEvent::Output(chunk) => Some(chunk.seq),
                    ExecProcessEvent::Exited { seq, .. } | ExecProcessEvent::Closed { seq } => {
                        Some(*seq)
                    }
                    ExecProcessEvent::Failed(_) => None,
                });
                let missing_sandbox_denial = matches!(
                    event.as_ref(),
                    Some(ExecProcessEvent::Exited {
                        sandbox_denied: None,
                        ..
                    })
                );
                if event.is_none()
                    || event_seq.is_some_and(|seq| seq > last_seq.saturating_add(1))
                    || missing_sandbox_denial
                {
                    // The final drain read waits for transport recovery and collects retained
                    // output before closing an idle session with inherited pipes still open.
                    let response = match process
                        .read(
                            Some(last_seq),
                            /*max_bytes*/ None,
                            /*wait_ms*/ Some(0),
                        )
                        .await
                    {
                        Ok(response) => response,
                        Err(err) => {
                            let state = state_tx.borrow().clone();
                            let _ = state_tx.send_replace(state.failed(err.to_string()));
                            output_closed.fail();
                            output_closed_notify.notify_waiters();
                            cancellation_token.cancel();
                            break;
                        }
                    };
                    let ExecReadResponse {
                        chunks,
                        next_seq,
                        exited,
                        exit_code,
                        closed,
                        failure,
                        sandbox_denied,
                    } = response;
                    for chunk in chunks.into_iter().filter(|chunk| chunk.seq > last_seq) {
                        let bytes = chunk.chunk.into_inner();
                        super::terminal_watch::publish_output(
                            &output_buffer,
                            &output_tx,
                            &terminal_watches,
                            chunk.stream,
                            bytes,
                        )
                        .await;
                        output_notify.notify_waiters();
                    }
                    last_seq = last_seq.max(next_seq.saturating_sub(1));
                    if let Some(message) = failure {
                        let state = state_tx.borrow().clone();
                        let _ = state_tx.send_replace(state.failed(message));
                        output_closed.fail();
                        output_closed_notify.notify_waiters();
                        cancellation_token.cancel();
                        break;
                    }
                    if sandbox_denied || exited {
                        let mut state = state_tx.borrow().clone();
                        state.sandbox_denied |= sandbox_denied;
                        let _ = state_tx.send_replace(if exited {
                            state.exited(exit_code)
                        } else {
                            state
                        });
                    }
                    if exited && output_drain_policy == OutputDrainPolicy::BoundedAfterExit {
                        cancellation_token.cancel();
                    }
                    if closed || final_drain {
                        if closed {
                            output_closed.store(true, Ordering::Release);
                        } else {
                            output_closed.interrupt();
                        }
                        output_closed_notify.notify_waiters();
                        cancellation_token.cancel();
                        break;
                    }
                    continue;
                }

                let Some(event) = event else {
                    continue;
                };
                match event {
                    ExecProcessEvent::Output(chunk) => {
                        if chunk.seq <= last_seq {
                            continue;
                        }
                        last_seq = chunk.seq;
                        let bytes = chunk.chunk.into_inner();
                        super::terminal_watch::publish_output(
                            &output_buffer,
                            &output_tx,
                            &terminal_watches,
                            chunk.stream,
                            bytes,
                        )
                        .await;
                        output_notify.notify_waiters();
                    }
                    ExecProcessEvent::Exited {
                        seq,
                        exit_code,
                        sandbox_denied,
                    } => {
                        if seq <= last_seq {
                            continue;
                        }
                        last_seq = seq;
                        let mut state = state_tx.borrow().clone();
                        state.sandbox_denied |= sandbox_denied.unwrap_or(false);
                        let _ = state_tx.send_replace(state.exited(Some(exit_code)));
                        if output_drain_policy == OutputDrainPolicy::BoundedAfterExit {
                            cancellation_token.cancel();
                        }
                    }
                    ExecProcessEvent::Closed { seq } => {
                        if seq <= last_seq {
                            continue;
                        }
                        output_closed.store(true, Ordering::Release);
                        output_closed_notify.notify_waiters();
                        cancellation_token.cancel();
                        break;
                    }
                    ExecProcessEvent::Failed(message) => {
                        let state = state_tx.borrow().clone();
                        let _ = state_tx.send_replace(state.failed(message));
                        output_closed.fail();
                        output_closed_notify.notify_waiters();
                        cancellation_token.cancel();
                        break;
                    }
                }
            }
            if final_drain {
                termination::request_backend(&termination, &ProcessHandle::ExecServer(process));
            }
            cancellation_token.cancel();
        })
    }

    fn spawn_local_output_task(
        mut stdout: tokio::sync::mpsc::Receiver<Vec<u8>>,
        mut stderr: tokio::sync::mpsc::Receiver<Vec<u8>>,
        output_handles: OutputHandles,
        output_tx: broadcast::Sender<Vec<u8>>,
    ) -> JoinHandle<()> {
        let OutputHandles {
            terminal_watches,
            output_buffer,
            output_notify,
            output_closed,
            output_closed_notify,
            ..
        } = output_handles;
        let output_task_guard = OutputTaskGuard {
            terminal_watches: Arc::clone(&terminal_watches),
            output_closed: Arc::clone(&output_closed),
            output_closed_notify: Arc::clone(&output_closed_notify),
        };
        tokio::spawn(async move {
            let _output_task_guard = output_task_guard;
            let mut stdout_open = true;
            let mut stderr_open = true;
            while stdout_open || stderr_open {
                let (stream, chunk) = tokio::select! {
                    chunk = stdout.recv(), if stdout_open => (ExecOutputStream::Stdout, chunk),
                    chunk = stderr.recv(), if stderr_open => (ExecOutputStream::Stderr, chunk),
                };
                if let Some(chunk) = chunk {
                    super::terminal_watch::publish_output(
                        &output_buffer,
                        &output_tx,
                        &terminal_watches,
                        stream,
                        chunk,
                    )
                    .await;
                    output_notify.notify_waiters();
                } else if stream == ExecOutputStream::Stdout {
                    stdout_open = false;
                } else {
                    stderr_open = false;
                }
            }
            output_closed.store(true, Ordering::Release);
        })
    }

    fn signal_exit(&self, exit_code: Option<i32>) {
        let state = self.state_rx.borrow().clone();
        let _ = self.state_tx.send_replace(state.exited(exit_code));
        self.output.cancellation_token.cancel();
    }
}

impl Drop for UnifiedExecProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[path = "process_termination.rs"]
mod termination;
