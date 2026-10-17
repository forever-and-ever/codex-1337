//! One terminal owner, ordered control output, and replaceable complete frames.
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::io::IsTerminal;
use std::io::Write;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::Weak;
use std::thread;
use std::time::Instant;

use crossterm::SynchronizedUpdate;
use crossterm::cursor::SetCursorStyle;
use ratatui::backend::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Position;
use ratatui::layout::Size;

use super::Terminal;
use crate::tui::FrameRequester;
use crate::ui_latency;
use crate::ui_latency::FrameStamp;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FrameId(u64);

struct Snapshot {
    buffer: Buffer,
    size: Size,
    cursor: Option<Position>,
    style: SetCursorStyle,
    invalidate: bool,
}

enum Operation {
    Bytes(Vec<u8>),
    Frame(Snapshot),
}

struct FrameJob {
    id: FrameId,
    operations: Vec<Operation>,
    stamp: Option<FrameStamp>,
}

enum Job {
    Control(Vec<u8>),
    Frame(FrameJob),
    Barrier,
    Stop,
}

struct Envelope {
    sequence: u64,
    job: Job,
}

struct Completion {
    id: FrameId,
    stamp: Option<FrameStamp>,
    phases: [(Instant, Instant); 3],
    ended_at: Instant,
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Writing,
    Failed(Arc<io::Error>),
    Stopped,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Envelope>,
    sequence: u64,
    completed_sequence: u64,
    next_frame: u64,
    phase: Phase,
    completed: VecDeque<Completion>,
    submitted: Option<FrameId>,
    presented: Option<FrameId>,
    ui: Option<UiLink>,
    waiting: bool,
}

struct UiLink {
    requester: FrameRequester,
    completed: tokio::sync::broadcast::Sender<()>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

static ACTIVE: Mutex<Option<Weak<Shared>>> = Mutex::new(None);
thread_local! {
    static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
    static IS_WRITER: Cell<bool> = const { Cell::new(false) };
}

fn active() -> Option<Arc<Shared>> {
    ACTIVE
        .lock()
        .expect("terminal output registry poisoned")
        .as_ref()?
        .upgrade()
}

fn check(state: &State) -> io::Result<()> {
    match &state.phase {
        Phase::Failed(error) => Err(io::Error::new(error.kind(), Arc::clone(error))),
        Phase::Stopped => Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "terminal output writer stopped",
        )),
        Phase::Idle | Phase::Writing => Ok(()),
    }
}

fn enqueue(shared: &Shared, job: Job) -> io::Result<u64> {
    let mut state = shared
        .state
        .lock()
        .expect("terminal output mailbox poisoned");
    check(&state)?;
    state.sequence += 1;
    let sequence = state.sequence;
    state.queue.push_back(Envelope { sequence, job });
    shared.changed.notify_one();
    Ok(sequence)
}

struct Owner {
    shared: Arc<Shared>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = enqueue(&self.shared, Job::Stop);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("terminal output writer panicked");
        }
    }
}

/// Synchronous without a live owner, including ordinary headless tests.
pub struct Output {
    _owner: Option<Owner>,
}

pub fn stdout() -> Output {
    Output { _owner: None }
}

impl Output {
    #[cfg(unix)]
    pub(crate) fn start(size: Size, cursor: Position) -> io::Result<Self> {
        #[cfg(test)]
        {
            let _ = (size, cursor);
            Ok(stdout())
        }
        #[cfg(not(test))]
        {
            let shared = Arc::new(Shared::default());
            let mut registry = ACTIVE.lock().expect("terminal output registry poisoned");
            assert!(
                registry.as_ref().and_then(Weak::upgrade).is_none(),
                "terminal output already owned"
            );
            let mailbox = Arc::clone(&shared);
            let terminal = Terminal::with_screen_size_and_cursor_position(
                CrosstermBackend::new(io::stdout()),
                size,
                cursor,
            );
            let worker = thread::Builder::new()
                .name("terminal-output".into())
                .spawn(move || run(mailbox, terminal))?;
            *registry = Some(Arc::downgrade(&shared));
            Ok(Self {
                _owner: Some(Owner {
                    shared,
                    worker: Some(worker),
                }),
            })
        }
    }

    pub fn is_terminal(&self) -> bool {
        io::stdout().is_terminal()
    }

    pub fn sync_update<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> io::Result<R> {
        let Some(shared) = active() else {
            return io::stdout().sync_update(|_| f(self));
        };
        drain_completions();
        check(
            &shared
                .state
                .lock()
                .expect("terminal output mailbox poisoned"),
        )?;
        // A nested update shares its outer frame's atomic handoff.
        if CAPTURE.with_borrow(|capture| capture.is_some()) {
            return Ok(f(self));
        }
        let id = {
            let mut state = shared
                .state
                .lock()
                .expect("terminal output mailbox poisoned");
            state.next_frame += 1;
            FrameId(state.next_frame)
        };
        CAPTURE.with_borrow_mut(|capture| {
            *capture = Some(Capture {
                id,
                operations: Vec::new(),
            })
        });
        let capture = CaptureGuard { shared };
        let result = f(self);
        capture.finish()?;
        Ok(result)
    }
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if IS_WRITER.get() {
            return io::stdout().write(bytes);
        }
        if CAPTURE.with_borrow_mut(|capture| {
            if let Some(capture) = capture {
                append_bytes(&mut capture.operations, bytes);
                true
            } else {
                false
            }
        }) {
            return Ok(bytes.len());
        }
        if let Some(shared) = active() {
            let mut state = shared
                .state
                .lock()
                .expect("terminal output mailbox poisoned");
            check(&state)?;
            if let Some(Envelope {
                job: Job::Control(pending),
                ..
            }) = state.queue.back_mut()
            {
                pending.extend_from_slice(bytes);
            } else {
                state.sequence += 1;
                let sequence = state.sequence;
                state.queue.push_back(Envelope {
                    sequence,
                    job: Job::Control(bytes.to_vec()),
                });
            }
            shared.changed.notify_one();
            Ok(bytes.len())
        } else {
            io::stdout().write(bytes)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if IS_WRITER.get() {
            return io::stdout().flush();
        }
        if let Some(shared) = active() {
            check(
                &shared
                    .state
                    .lock()
                    .expect("terminal output mailbox poisoned"),
            )
        } else {
            io::stdout().flush()
        }
    }
}

fn append_bytes(operations: &mut Vec<Operation>, bytes: &[u8]) {
    if let Some(Operation::Bytes(pending)) = operations.last_mut() {
        pending.extend_from_slice(bytes);
    } else {
        operations.push(Operation::Bytes(bytes.to_vec()));
    }
}

struct Capture {
    id: FrameId,
    operations: Vec<Operation>,
}

struct CaptureGuard {
    shared: Arc<Shared>,
}

impl CaptureGuard {
    fn finish(self) -> io::Result<()> {
        self.publish()
    }

    fn publish(&self) -> io::Result<()> {
        let Some(Capture { id, mut operations }) = CAPTURE.with_borrow_mut(Option::take) else {
            return Ok(());
        };
        if !operations
            .iter()
            .any(|op| matches!(op, Operation::Frame(_)))
        {
            let mut bytes = b"\x1b[?2026h".to_vec();
            for operation in operations {
                if let Operation::Bytes(part) = operation {
                    bytes.extend(part);
                }
            }
            bytes.extend_from_slice(b"\x1b[?2026l");
            enqueue(&self.shared, Job::Control(bytes))?;
            return Ok(());
        }
        let mut stamp = ui_latency::frame_submitted();
        let mut state = self
            .shared
            .state
            .lock()
            .expect("terminal output mailbox poisoned");
        check(&state)?;
        state.submitted = Some(id);
        // Controls keep their place; only an explicit flush fence protects an older frame.
        let pending = state
            .queue
            .iter()
            .enumerate()
            .rev()
            .take_while(|(_, entry)| !matches!(entry.job, Job::Barrier | Job::Stop))
            .find_map(|(index, entry)| matches!(entry.job, Job::Frame(_)).then_some(index));
        if let Some(index) = pending {
            let Envelope {
                sequence,
                job: Job::Frame(old),
            } = state.queue.remove(index).expect("pending frame vanished")
            else {
                unreachable!()
            };
            stamp = ui_latency::merge_frame_stamps(old.stamp, stamp);
            let invalidated = old
                .operations
                .iter()
                .any(|op| matches!(op, Operation::Frame(frame) if frame.invalidate));
            let mut retained = Vec::new();
            for operation in old.operations {
                if let Operation::Bytes(bytes) = operation {
                    retained.extend(bytes);
                }
            }
            if !retained.is_empty() {
                state.queue.insert(
                    index,
                    Envelope {
                        sequence,
                        job: Job::Control(retained),
                    },
                );
            }
            if invalidated {
                for op in &mut operations {
                    if let Operation::Frame(frame) = op {
                        frame.invalidate = true;
                    }
                }
            }
        }
        state.sequence += 1;
        let sequence = state.sequence;
        state.queue.push_back(Envelope {
            sequence,
            job: Job::Frame(FrameJob {
                id,
                operations,
                stamp,
            }),
        });
        self.shared.changed.notify_one();
        Ok(())
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        // A panic may have changed terminal modes before rendering failed. Preserve those
        // ordered operations; the panic hook's restoration barrier can publish them too.
        if let Err(error) = self.publish() {
            tracing::error!(%error, "failed to hand terminal frame to writer");
        }
    }
}

pub(super) fn capture_frame(
    buffer: &Buffer,
    previous: &Buffer,
    size: Size,
    cursor: Option<Position>,
    style: SetCursorStyle,
) -> bool {
    CAPTURE.with_borrow_mut(|capture| {
        let Some(capture) = capture else {
            return false;
        };
        capture.operations.push(Operation::Frame(Snapshot {
            buffer: buffer.clone(),
            size,
            cursor,
            style,
            invalidate: previous
                .content
                .iter()
                .any(|cell| cell.diff_option == CellDiffOption::AlwaysUpdate),
        }));
        true
    })
}

pub(crate) fn is_active() -> bool {
    active().is_some()
}

pub(crate) fn query(fallback: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    if is_active() {
        stdout().write_all(bytes)?;
        barrier()
    } else {
        fallback.write_all(bytes)?;
        fallback.flush()
    }
}

pub(crate) fn bind(requester: FrameRequester) {
    if let Some(shared) = active() {
        let mut state = shared
            .state
            .lock()
            .expect("terminal output mailbox poisoned");
        state.ui = Some(UiLink {
            requester,
            completed: tokio::sync::broadcast::channel(16).0,
        });
    }
}

pub(crate) fn completion_events() -> Option<tokio::sync::broadcast::Receiver<()>> {
    let shared = active()?;
    let state = shared
        .state
        .lock()
        .expect("terminal output mailbox poisoned");
    state.ui.as_ref().map(|ui| ui.completed.subscribe())
}

pub(crate) async fn next_completion(receiver: &mut Option<tokio::sync::broadcast::Receiver<()>>) {
    match receiver {
        Some(channel) => match channel.recv().await {
            Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => *receiver = None,
        },
        None => {}
    }
    std::future::pending::<()>().await;
}

pub(crate) fn current_frame() -> Option<FrameId> {
    CAPTURE.with_borrow(|capture| capture.as_ref().map(|capture| capture.id))
}

#[cfg(test)]
pub(crate) fn submitted_frame() -> Option<FrameId> {
    active()?
        .state
        .lock()
        .expect("terminal output mailbox poisoned")
        .submitted
}

pub(crate) fn presented_frame() -> Option<FrameId> {
    active()?
        .state
        .lock()
        .expect("terminal output mailbox poisoned")
        .presented
}

pub(crate) fn paint_ready() -> bool {
    drain_completions();
    let Some(shared) = active() else { return true };
    let mut state = shared
        .state
        .lock()
        .expect("terminal output mailbox poisoned");
    let ready = matches!(state.phase, Phase::Failed(_) | Phase::Stopped)
        || (matches!(state.phase, Phase::Idle) && state.queue.is_empty());
    state.waiting |= !ready;
    ready
}

fn drain_completions() {
    let Some(shared) = active() else { return };
    let completed = std::mem::take(
        &mut shared
            .state
            .lock()
            .expect("terminal output mailbox poisoned")
            .completed,
    );
    for completion in completed {
        ui_latency::frame_completed(completion.stamp, completion.phases, completion.ended_at);
    }
}

pub(crate) fn completion_received() {
    drain_completions();
    let Some(shared) = active() else { return };
    let mut state = shared
        .state
        .lock()
        .expect("terminal output mailbox poisoned");
    let failed = matches!(state.phase, Phase::Failed(_));
    if failed || (state.waiting && matches!(state.phase, Phase::Idle) && state.queue.is_empty()) {
        state.waiting = false;
        if let Some(ui) = &state.ui {
            ui.requester.schedule_frame();
        }
    }
}

/// A writer panic must not make the global panic hook join its own flush barrier.
pub(crate) fn prepare_panic() {
    if IS_WRITER.get()
        && let Some(shared) = active()
    {
        if let Ok(mut state) = shared.state.try_lock() {
            state.phase = Phase::Failed(Arc::new(io::Error::other(
                "terminal output writer panicked",
            )));
            if let Some(ui) = &state.ui {
                let _ = ui.completed.send(());
            }
        }
        shared.changed.notify_all();
    }
}

pub(crate) fn restoration_barrier() -> io::Result<()> {
    // Editor handoff restores modes inside a synchronized frame. Its outer barrier
    // transfers ownership only after that whole update, not halfway through it.
    if current_frame().is_some() {
        Ok(())
    } else {
        barrier()
    }
}

/// Exceptional ownership transfer: every preceding byte has reached the terminal.
pub(crate) fn barrier() -> io::Result<()> {
    if IS_WRITER.get() {
        return io::stdout().flush();
    }
    let Some(shared) = active() else {
        return io::stdout().flush();
    };
    CaptureGuard {
        shared: Arc::clone(&shared),
    }
    .publish()?;
    let target = enqueue(&shared, Job::Barrier)?;
    let mut state = shared
        .state
        .lock()
        .expect("terminal output mailbox poisoned");
    while state.completed_sequence < target {
        check(&state)?;
        state = shared
            .changed
            .wait(state)
            .expect("terminal output mailbox poisoned");
    }
    check(&state)?;
    drop(state);
    drain_completions();
    Ok(())
}

fn write_snapshot<B: Backend<Error = io::Error> + Write>(
    terminal: &mut Terminal<B>,
    snapshot: Snapshot,
) -> io::Result<[(Instant, Instant); 3]> {
    terminal.resize(snapshot.size)?;
    terminal.set_viewport_area(snapshot.buffer.area);
    *terminal.current_buffer_mut() = snapshot.buffer;
    if snapshot.invalidate {
        terminal.invalidate_viewport();
    }
    // Control batches can change cursor state independently of the buffer.
    terminal.invalidate_cursor_state();
    let diff_start = Instant::now();
    let updates = super::diff_buffers(terminal.previous_buffer(), terminal.current_buffer());
    let diff_end = Instant::now();
    let write_start = diff_end;
    if !updates.is_empty() && !terminal.hidden_cursor {
        terminal.hide_cursor()?;
    }
    terminal.flush_updates(updates)?;
    match snapshot.cursor {
        None if !terminal.hidden_cursor => terminal.hide_cursor()?,
        None => {}
        Some(position) => {
            terminal.set_cursor_style_with_repair(snapshot.style)?;
            terminal.set_cursor_position(position)?;
            terminal.show_cursor()?;
        }
    }
    terminal.swap_buffers();
    let write_end = Instant::now();
    Backend::flush(terminal.backend_mut())?;
    let flush_end = Instant::now();
    Ok([
        (diff_start, diff_end),
        (write_start, write_end),
        (write_end, flush_end),
    ])
}

fn run<B: Backend<Error = io::Error> + Write>(shared: Arc<Shared>, mut terminal: Terminal<B>) {
    IS_WRITER.set(true);
    loop {
        let envelope = {
            let mut state = shared
                .state
                .lock()
                .expect("terminal output mailbox poisoned");
            while state.queue.is_empty() {
                state = shared
                    .changed
                    .wait(state)
                    .expect("terminal output mailbox poisoned");
            }
            state.phase = Phase::Writing;
            state
                .queue
                .pop_front()
                .expect("terminal writer woke without work")
        };
        let mut completion = None;
        let stop = matches!(envelope.job, Job::Stop);
        let result = match envelope.job {
            Job::Control(bytes) => terminal
                .backend_mut()
                .write_all(&bytes)
                .and_then(|_| Write::flush(terminal.backend_mut())),
            Job::Frame(frame) => {
                let now = Instant::now();
                let mut phases = [(now, now); 3];
                let result = terminal
                    .backend_mut()
                    .write_all(b"\x1b[?2026h")
                    .and_then(|_| {
                        for operation in frame.operations {
                            match operation {
                                Operation::Bytes(bytes) => {
                                    terminal.backend_mut().write_all(&bytes)?
                                }
                                Operation::Frame(snapshot) => {
                                    phases = write_snapshot(&mut terminal, snapshot)?
                                }
                            }
                        }
                        Ok(())
                    });
                // End synchronized output even after a failed frame.
                let ended = terminal
                    .backend_mut()
                    .write_all(b"\x1b[?2026l")
                    .and_then(|_| Write::flush(terminal.backend_mut()));
                let ended_at = Instant::now();
                phases[2].1 = ended_at;
                if result.is_ok() && ended.is_ok() {
                    completion = Some(Completion {
                        id: frame.id,
                        stamp: frame.stamp,
                        phases,
                        ended_at,
                    });
                }
                result.and(ended)
            }
            Job::Barrier | Job::Stop => Write::flush(terminal.backend_mut()),
        };
        let mut state = shared
            .state
            .lock()
            .expect("terminal output mailbox poisoned");
        state.phase = match result {
            Err(error) => Phase::Failed(Arc::new(error)),
            Ok(()) if stop => Phase::Stopped,
            Ok(()) => Phase::Idle,
        };
        state.completed_sequence = envelope.sequence;
        let frame_completed = completion.is_some();
        if let Some(completion) = completion {
            state.presented = Some(completion.id);
            state.completed.push_back(completion);
        }
        if let Some(ui) = &state.ui
            && (frame_completed
                || matches!(state.phase, Phase::Failed(_))
                || state.queue.is_empty())
        {
            let _ = ui.completed.send(());
        }
        shared.changed.notify_all();
        if stop || matches!(state.phase, Phase::Failed(_)) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::time::Duration;

    struct BlockedOutput {
        bytes: Arc<Mutex<Vec<u8>>>,
        block: Arc<AtomicBool>,
        entered: mpsc::SyncSender<()>,
        resume: mpsc::Receiver<()>,
    }

    impl Write for BlockedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.block.swap(false, Ordering::AcqRel) {
                self.entered.send(()).expect("UI still present");
                self.resume.recv().expect("UI releases terminal");
            }
            self.bytes
                .lock()
                .expect("capture poisoned")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_writer_keeps_ui_live_and_preserves_latest_frame_and_barrier_order() -> io::Result<()>
    {
        let shared = Arc::new(Shared::default());
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let block = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        let size = Size::new(3, 1);
        let terminal = Terminal::with_screen_size_and_cursor_position(
            CrosstermBackend::new(BlockedOutput {
                bytes: Arc::clone(&bytes),
                block: Arc::clone(&block),
                entered: entered_tx,
                resume: resume_rx,
            }),
            size,
            Position::ORIGIN,
        );
        *ACTIVE.lock().expect("registry poisoned") = Some(Arc::downgrade(&shared));
        let mailbox = Arc::clone(&shared);
        let owner = Owner {
            shared: Arc::clone(&shared),
            worker: Some(thread::spawn(move || run(mailbox, terminal))),
        };
        let (requester, mut draws) = FrameRequester::test_channel();
        bind(requester);
        let mut completions = completion_events().expect("writer completion stream");
        let view = RefCell::new(crate::transcript_view::TranscriptView::default());
        let submit = |text: &str, width: u16| -> io::Result<()> {
            let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 3, 1));
            stdout().sync_update(|_| {
                view.borrow_mut().render(
                    ratatui::layout::Rect::new(0, 0, width, 1),
                    &mut buffer,
                    &[],
                );
                buffer.set_string(0, 0, text, ratatui::style::Style::default());
                assert!(capture_frame(
                    &buffer,
                    &Buffer::empty(buffer.area),
                    size,
                    None,
                    SetCursorStyle::DefaultUserShape
                ));
            })
        };
        submit("abc", 3)?;
        barrier()?;
        assert!(
            completions.try_recv().is_ok(),
            "completion wakes the UI separately"
        );
        completion_received();
        assert!(
            draws.try_recv().is_err(),
            "an idle completion must not start a repaint loop"
        );
        view.borrow_mut().promote_presented_frame();
        assert_eq!(view.borrow().remaining_area().width, 3);
        let initial = presented_frame().expect("initial completed frame");
        block.store(true, Ordering::Release);
        stdout().write_all(b"\x1b]0;before\x07")?;
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("writer blocked");
        assert!(
            !paint_ready(),
            "the UI skips paint instead of joining blocked I/O"
        );
        submit("xbc", 2)?;
        let skipped = submitted_frame().expect("pending frame");
        stdout().write_all(b"\x1b]0;middle\x07")?;
        submit("xby", 1)?;
        let latest = submitted_frame().expect("latest frame");
        assert_ne!(skipped, latest);
        view.borrow_mut().promote_presented_frame();
        assert_eq!(
            view.borrow().remaining_area().width,
            3,
            "routing retains completed geometry while output is blocked"
        );
        assert_eq!(
            presented_frame(),
            Some(initial),
            "submission is not presentation"
        );
        {
            let state = shared.state.lock().expect("mailbox poisoned");
            assert_eq!(
                state
                    .queue
                    .iter()
                    .filter(|entry| matches!(entry.job, Job::Frame(_)))
                    .count(),
                1,
                "latest-wins slot never queues stale frames across controls"
            );
        }
        stdout().write_all(b"\x1b]0;after\x07")?;
        let (barrier_tx, barrier_rx) = mpsc::sync_channel(1);
        let waiter = thread::spawn(move || barrier_tx.send(barrier()).expect("barrier observed"));
        {
            let mut state = shared.state.lock().expect("mailbox poisoned");
            while !state
                .queue
                .iter()
                .any(|entry| matches!(entry.job, Job::Barrier))
            {
                let (next, timeout) = shared
                    .changed
                    .wait_timeout(state, Duration::from_secs(2))
                    .expect("mailbox poisoned");
                assert!(!timeout.timed_out(), "barrier was queued");
                state = next;
            }
        }
        assert!(
            barrier_rx.try_recv().is_err(),
            "barrier cannot overtake blocked output"
        );
        resume_tx.send(()).expect("writer waiting");
        barrier_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("barrier completed")?;
        waiter.join().expect("barrier waiter");
        completion_received();
        assert!(
            draws.try_recv().is_ok(),
            "completion retries a paint refused by backpressure"
        );
        completion_received();
        assert!(draws.try_recv().is_err(), "a refused paint wakes only once");
        assert!(paint_ready());
        assert_eq!(presented_frame(), Some(latest));
        view.borrow_mut().promote_presented_frame();
        assert_eq!(
            view.borrow().remaining_area().width,
            1,
            "only the completed latest frame promotes its geometry"
        );
        let output = bytes.lock().expect("capture poisoned").clone();
        let before = output
            .windows(9)
            .position(|part| part == b"0;before\x07")
            .expect("before title");
        let after = output
            .windows(8)
            .position(|part| part == b"0;after\x07")
            .expect("after title");
        let middle = output
            .windows(9)
            .position(|part| part == b"0;middle\x07")
            .expect("middle title");
        assert!(before < middle && middle < after);
        let frame_output = &output[before..after];
        assert!(frame_output.windows(8).any(|part| part == b"\x1b[?2026h"));
        assert!(frame_output.windows(8).any(|part| part == b"\x1b[?2026l"));
        let mut parser = vt100::Parser::new(1, 3, 0);
        parser.process(&output);
        assert_eq!(
            parser.screen().contents(),
            "xby",
            "diff against written abc, not skipped xbc"
        );
        drop(owner);
        Ok(())
    }
}
