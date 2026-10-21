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
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::layout::Size;

use super::Terminal;
use crate::tui::FrameRequester;
use crate::ui_latency;
use crate::ui_latency::FrameStamp;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FrameId(u64);

/// UI-side clears survive until their next captured frame, including frame replacement.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub(super) struct Invalidation(bool);

impl Invalidation {
    pub(super) fn mark(&mut self) {
        self.0 = true;
    }
    pub(super) fn take(&mut self) -> bool {
        std::mem::take(&mut self.0)
    }
    pub(super) fn is_marked(&self) -> bool {
        self.0
    }
}

struct Snapshot {
    buffer: Buffer,
    size: Size,
    cursor: Option<Position>,
    style: SetCursorStyle,
    invalidate: bool,
}

/// Cells painted over the presented frame, which otherwise stands.
struct Patch {
    cells: Buffer,
    screen: Size,
    viewport: Rect,
    cursor: Option<Position>,
    style: SetCursorStyle,
}

enum Operation {
    Bytes(Vec<u8>),
    Frame(Snapshot),
    Patch(Patch),
}

struct FrameJob {
    id: FrameId,
    operations: Vec<Operation>,
    stamp: Option<FrameStamp>,
}

enum Job {
    Control(Vec<u8>),
    Frame(FrameJob),
    /// An update over the presented frame. Unlike a frame it keeps its place: a later frame
    /// paints over it rather than replacing it.
    Patch(Vec<Operation>),
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

#[expect(
    clippy::expect_used,
    reason = "A prior panic while mutating output ordering state makes the shared mailbox unusable."
)]
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
    let mut state = shared.state.lock().map_err(mailbox_poisoned)?;
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
    #[expect(
        clippy::expect_used,
        reason = "The owned terminal writer must complete without panicking before its owner is released."
    )]
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
    #[cfg_attr(
        test,
        expect(
            clippy::unnecessary_wraps,
            reason = "The headless test implementation retains the fallible terminal-owner construction interface."
        )
    )]
    pub(crate) fn start(size: Size, cursor: Position) -> io::Result<Self> {
        #[cfg(test)]
        {
            let _ = (size, cursor);
            Ok(stdout())
        }
        #[cfg(not(test))]
        {
            let shared = Arc::new(Shared::default());
            let mut registry = ACTIVE
                .lock()
                .map_err(|_| io::Error::other("terminal output registry poisoned"))?;
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
                .spawn(move || run(&mailbox, terminal))?;
            *registry = Some(Arc::downgrade(&shared));
            Ok(Self {
                _owner: Some(Owner {
                    shared,
                    worker: Some(worker),
                }),
            })
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "Output replaces the stdout writer at existing call sites; terminal detection stays an instance operation on that sink."
    )]
    pub fn is_terminal(&self) -> bool {
        io::stdout().is_terminal()
    }

    pub fn sync_update<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> io::Result<R> {
        let Some(shared) = active() else {
            return io::stdout().sync_update(|_| f(self));
        };
        drain_completions();
        check(&*shared.state.lock().map_err(mailbox_poisoned)?)?;
        // A nested update shares its outer frame's atomic handoff.
        if CAPTURE.with_borrow(Option::is_some) {
            return Ok(f(self));
        }
        let id = {
            let mut state = shared.state.lock().map_err(mailbox_poisoned)?;
            state.next_frame += 1;
            FrameId(state.next_frame)
        };
        CAPTURE.with_borrow_mut(|capture| {
            *capture = Some(Capture {
                id,
                operations: Vec::new(),
            });
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
            let mut state = shared.state.lock().map_err(mailbox_poisoned)?;
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
            check(&*shared.state.lock().map_err(mailbox_poisoned)?)
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

    #[expect(
        clippy::expect_used,
        reason = "The exclusively locked queue still contains the pending frame selected immediately above."
    )]
    fn publish(&self) -> io::Result<()> {
        let Some(Capture { id, mut operations }) = CAPTURE.with_borrow_mut(Option::take) else {
            return Ok(());
        };
        if !operations
            .iter()
            .any(|op| matches!(op, Operation::Frame(_)))
        {
            if operations
                .iter()
                .any(|op| matches!(op, Operation::Patch(_)))
            {
                enqueue(&self.shared, Job::Patch(operations))?;
                return Ok(());
            }
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
        let mut state = self.shared.state.lock().map_err(mailbox_poisoned)?;
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
    invalidate: bool,
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
            invalidate,
        }));
        true
    })
}

pub(super) fn capture_patch(
    cells: &Buffer,
    screen: Size,
    viewport: Rect,
    cursor: Option<Position>,
    style: SetCursorStyle,
) -> bool {
    CAPTURE.with_borrow_mut(|capture| {
        let Some(capture) = capture else {
            return false;
        };
        capture.operations.push(Operation::Patch(Patch {
            cells: cells.clone(),
            screen,
            viewport,
            cursor,
            style,
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

#[expect(
    clippy::expect_used,
    reason = "A prior panic while mutating output ordering state makes the shared mailbox unusable."
)]
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

#[expect(
    clippy::expect_used,
    reason = "A prior panic while mutating output ordering state makes the shared mailbox unusable."
)]
pub(crate) fn completion_events() -> Option<tokio::sync::broadcast::Receiver<()>> {
    let shared = active()?;
    let state = shared
        .state
        .lock()
        .expect("terminal output mailbox poisoned");
    state.ui.as_ref().map(|ui| ui.completed.subscribe())
}

pub(crate) async fn next_completion(receiver: &mut Option<tokio::sync::broadcast::Receiver<()>>) {
    if let Some(channel) = receiver {
        match channel.recv().await {
            Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => *receiver = None,
        }
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

#[expect(
    clippy::expect_used,
    reason = "A prior panic while mutating output ordering state makes the shared mailbox unusable."
)]
pub(crate) fn presented_frame() -> Option<FrameId> {
    active()?
        .state
        .lock()
        .expect("terminal output mailbox poisoned")
        .presented
}

#[expect(
    clippy::expect_used,
    reason = "A prior panic while mutating output ordering state makes the shared mailbox unusable."
)]
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

#[expect(
    clippy::expect_used,
    reason = "A prior panic while mutating output ordering state makes the shared mailbox unusable."
)]
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

#[expect(
    clippy::expect_used,
    reason = "A prior panic while mutating output ordering state makes the shared mailbox unusable."
)]
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
            // A refused paint still owes what it owed; only a failure calls for fresh content.
            if failed {
                ui.requester.schedule_frame();
            } else {
                ui.requester.retry_paint();
            }
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
    let mut state = shared.state.lock().map_err(mailbox_poisoned)?;
    while state.completed_sequence < target {
        check(&state)?;
        state = shared.changed.wait(state).map_err(mailbox_poisoned)?;
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
    // Buffer::resize preserves flat cell indexes, not rows. Neither a moved
    // viewport nor a changed screen has a meaningful previous diff buffer.
    let geometry_changed = terminal.viewport_area != snapshot.buffer.area
        || terminal.last_known_screen_size != snapshot.size;
    terminal.resize(snapshot.size)?;
    terminal.set_viewport_area(snapshot.buffer.area);
    *terminal.current_buffer_mut() = snapshot.buffer;
    if snapshot.invalidate || geometry_changed {
        terminal.invalidate_viewport();
    }
    // Control batches can change cursor state independently of the buffer.
    terminal.invalidate_cursor_state();
    let diff_start = Instant::now();
    let updates = super::diff_buffers(terminal.previous_buffer(), terminal.current_buffer());
    let diff_end = Instant::now();
    let write_start = diff_end;
    terminal.present(updates, snapshot.cursor, snapshot.style)?;
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

fn write_patch<B: Backend<Error = io::Error> + Write>(
    terminal: &mut Terminal<B>,
    patch: &Patch,
) -> io::Result<()> {
    // A patch paints over the frame it was drawn against; any other screen awaits a full frame.
    if terminal.viewport_area != patch.viewport || terminal.last_known_screen_size != patch.screen {
        return Ok(());
    }
    // Control batches can change cursor state independently of the buffer.
    terminal.invalidate_cursor_state();
    terminal.paint_patch(&patch.cells, patch.cursor, patch.style)
}

/// Writes one synchronized update, with its frame's phases when it carried a frame.
fn write_update<B: Backend<Error = io::Error> + Write>(
    terminal: &mut Terminal<B>,
    operations: Vec<Operation>,
) -> io::Result<Option<[(Instant, Instant); 3]>> {
    let mut phases = None;
    let written = terminal
        .backend_mut()
        .write_all(b"\x1b[?2026h")
        .and_then(|()| {
            for operation in operations {
                match operation {
                    Operation::Bytes(bytes) => terminal.backend_mut().write_all(&bytes)?,
                    Operation::Frame(snapshot) => {
                        phases = Some(write_snapshot(terminal, snapshot)?);
                    }
                    Operation::Patch(patch) => write_patch(terminal, &patch)?,
                }
            }
            Ok(())
        });
    // End synchronized output even after a failed update.
    let ended = terminal
        .backend_mut()
        .write_all(b"\x1b[?2026l")
        .and_then(|()| Write::flush(terminal.backend_mut()));
    written.and(ended).map(|()| phases)
}

#[expect(
    clippy::expect_used,
    reason = "The writer owns an unpoisoned mailbox and removes work only after its queue is nonempty."
)]
fn run<B: Backend<Error = io::Error> + Write>(shared: &Shared, mut terminal: Terminal<B>) {
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
                .and_then(|()| Write::flush(terminal.backend_mut())),
            Job::Frame(frame) => {
                let started = Instant::now();
                let written = write_update(&mut terminal, frame.operations);
                let ended_at = Instant::now();
                if let Ok(phases) = &written {
                    let mut phases = phases.unwrap_or([(started, started); 3]);
                    phases[2].1 = ended_at;
                    completion = Some(Completion {
                        id: frame.id,
                        stamp: frame.stamp,
                        phases,
                        ended_at,
                    });
                }
                written.map(|_| ())
            }
            Job::Patch(operations) => write_update(&mut terminal, operations).map(|_| ()),
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

impl<B> Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    pub(super) fn capture_output_frame(
        &mut self,
        screen_size: Size,
        cursor_position: Option<Position>,
        cursor_style: SetCursorStyle,
    ) -> bool {
        let invalidate = self.output_invalidation.take();
        if capture_frame(
            self.current_buffer(),
            invalidate,
            screen_size,
            cursor_position,
            cursor_style,
        ) {
            self.last_known_cursor_pos = cursor_position.unwrap_or(self.last_known_cursor_pos);
            self.hidden_cursor = cursor_position.is_none();
            self.swap_buffers();
            return true;
        }

        false
    }

    /// Repaints a frame bound when it was drawn, as the editor handoff repaints the screen it
    /// leaves. Binding is not idempotent: at 256 colors a bound cell holds a fixed xterm index
    /// that may equal a token, so the frame reaches the screen as it is, never bound again. It
    /// was bound under the pack's default colors, which the handoff has given back to the
    /// terminal, so they are published again first, ahead of the frame in the writer's order.
    pub(crate) fn replay(&mut self, frame: &Buffer) -> io::Result<()> {
        crate::warpaint::invest(&mut self.backend)?;
        let screen_size = self.size()?;
        if screen_size != self.last_known_screen_size {
            self.resize(screen_size)?;
        }
        let area = self.viewport_area.intersection(frame.area);
        let buffer = self.current_buffer_mut();
        for position in area.positions() {
            buffer[position] = frame[position].clone();
        }
        if self.capture_output_frame(
            screen_size,
            /*cursor_position*/ None,
            SetCursorStyle::DefaultUserShape,
        ) {
            return Ok(());
        }
        if !self.hidden_cursor {
            self.hide_cursor()?;
        }
        self.flush()?;
        self.swap_buffers();
        Backend::flush(&mut self.backend)
    }

    pub(super) fn output_cursor_position(&mut self) -> io::Result<Position> {
        if is_active() {
            return crate::terminal_probe::cursor_position(crate::terminal_probe::DEFAULT_TIMEOUT)?
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "terminal cursor query timed out")
                });
        }
        self.backend.get_cursor_position()
    }
}

pub(super) fn is_painted_column(x: u16, left: u16, last_nonblank: u16) -> bool {
    x - left <= last_nonblank
}

pub(crate) fn begin_restoration(restore: impl FnOnce() -> io::Result<()>) -> Option<io::Error> {
    let mut first_error = restoration_barrier().err();
    if let Err(error) = restore() {
        first_error.get_or_insert(error);
    }
    first_error
}

pub(crate) fn finish_restoration(first_error: &mut Option<io::Error>) {
    if let Err(error) = restoration_barrier() {
        first_error.get_or_insert(error);
    }
}

pub(crate) fn panic_barrier() {
    prepare_panic();
    let _ = barrier();
}

fn mailbox_poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("terminal output mailbox poisoned")
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
        use crate::test_backend::VT100Backend;
        use ratatui::layout::Rect;
        use ratatui::style::Style;

        fn capture(
            ui: &mut Terminal<VT100Backend>,
            buffer: Buffer,
            size: Size,
        ) -> io::Result<Snapshot> {
            ui.set_viewport_area(buffer.area);
            let operations = captured(ui, |ui| {
                ui.draw_with_size(size, |frame| {
                    *frame.buffer_mut() = buffer;
                })
            })?;
            let mut frames = operations
                .into_iter()
                .filter_map(|operation| match operation {
                    Operation::Frame(frame) => Some(frame),
                    Operation::Bytes(_) | Operation::Patch(_) => None,
                });
            let snapshot = frames.next().expect("one frame");
            assert!(frames.next().is_none());
            Ok(snapshot)
        }

        /// What `present` hands the writer, in order.
        fn captured<B: Backend<Error = io::Error> + Write>(
            ui: &mut Terminal<B>,
            present: impl FnOnce(&mut Terminal<B>) -> io::Result<()>,
        ) -> io::Result<Vec<Operation>> {
            CAPTURE.with_borrow_mut(|capture| {
                *capture = Some(Capture {
                    id: FrameId(0),
                    operations: Vec::new(),
                });
            });
            present(ui)?;
            let capture = CAPTURE
                .with_borrow_mut(Option::take)
                .expect("captured draw");
            Ok(capture.operations)
        }

        fn rule(area: Rect) -> Buffer {
            let mut buffer = Buffer::empty(area);
            buffer.set_string(
                area.x,
                area.y + 1,
                "─".repeat(usize::from(area.width)),
                Style::default(),
            );
            buffer
        }

        fn assert_rule(writer: &Terminal<VT100Backend>, area: Rect) {
            let contents = writer.backend().vt100().screen().contents();
            let row: String = contents
                .lines()
                .nth(usize::from(area.y + 1))
                .unwrap_or("")
                .chars()
                .skip(usize::from(area.x))
                .take(usize::from(area.width))
                .collect();
            assert_eq!(row, "─".repeat(usize::from(area.width)));
            assert!(
                contents.lines().nth(2).unwrap_or("").trim().is_empty(),
                "resize paints blank, never clipped old-width content"
            );
        }

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
            worker: Some(thread::spawn(move || run(&mailbox, terminal))),
        };
        let (requester, mut draws) = FrameRequester::test_channel();
        bind(requester);
        let mut completions = completion_events().expect("writer completion stream");
        let view = RefCell::new(crate::transcript_view::TranscriptView::default());
        let submit = |text: &str, width: u16| -> io::Result<()> {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
            stdout().sync_update(|_| {
                view.borrow_mut()
                    .render(Rect::new(0, 0, width, 1), &mut buffer, &[]);
                buffer.set_string(0, 0, text, Style::default());
                assert!(capture_frame(
                    &buffer,
                    false,
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

        // The writer and UI own distinct diff buffers, even when a UI clear has
        // already erased the physical screen. Buffer::resize must not turn a
        // 120-column rule into only the 24 columns whose flat indexes differ.
        let screen = Size::new(120, 4);
        let mut ui = Terminal::with_options(VT100Backend::new(120, 4))?;
        let mut writer = Terminal::with_options(VT100Backend::new(120, 4))?;
        let mut initial = rule(Rect::new(0, 0, 120, 4));
        initial.set_string(0, 2, "old-width transcript rows", Style::default());
        write_snapshot(&mut writer, capture(&mut ui, initial, screen)?)?;

        // Each geometry axis independently makes a diff invalid: width, viewport
        // origin, and screen size with an otherwise identical viewport.
        let area = Rect::new(1, 0, 95, 4);
        for (viewport, size) in [
            (Rect::new(0, 0, 96, 4), Size::new(96, 4)),
            (area, Size::new(96, 4)),
            (area, screen),
        ] {
            writer
                .backend_mut()
                .clear_region(ratatui::backend::ClearType::All)?;
            let snapshot = capture(&mut ui, rule(viewport), size)?;
            assert!(
                !snapshot.invalidate,
                "geometry is checked by the writer itself"
            );
            write_snapshot(&mut writer, snapshot)?;
            assert_rule(&writer, viewport);
        }

        for clear in 0..4 {
            match clear {
                0 => ui.clear_after_position(Position::ORIGIN)?,
                1 => ui.clear_visible_screen()?,
                2 => ui.clear_scrollback_and_visible_screen_ansi()?,
                _ => ui.invalidate_viewport(),
            }
            // Raw clear bytes reach the physical terminal, not its writer-side
            // diff buffer. The next frame must carry the clear explicitly.
            writer
                .backend_mut()
                .clear_region(ratatui::backend::ClearType::All)?;
            let snapshot = capture(&mut ui, rule(area), screen)?;
            assert!(snapshot.invalidate, "clear must survive capture");
            write_snapshot(&mut writer, snapshot)?;
            assert_rule(&writer, area);
            assert!(
                !capture(&mut ui, rule(area), screen)?.invalidate,
                "clear is consumed once"
            );
        }

        // The editor handoff gives the terminal its own default colors back, then repaints the
        // frame on screen as it was bound. At 256 colors a bound cell holds a fixed xterm index
        // that may equal a token, here the canvas's, so the frame reaches the writer as it is,
        // never bound again; and an invested frame paints the pack's ink and canvas as `Reset`,
        // so the UI's terminal publishes the pack's colors again, ahead of the frame.
        let own = crate::terminal_probe::DefaultColors {
            fg: (0xe0, 0xe1, 0xe2),
            bg: (0x10, 0x11, 0x12),
        };
        let published = |fg: (u8, u8, u8), bg: (u8, u8, u8)| {
            [(10, fg), (11, bg)]
                .map(|(slot, (r, g, b))| format!("\x1b]{slot};rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\"))
                .concat()
        };
        crate::warpaint::note_terminal_colors(Some(own));
        crate::terminal_palette::with_test_default_colors(own, || -> io::Result<()> {
            let ink = crate::warpaint::rgb(crate::warpaint::Figure::Ink.color()).expect("ink");
            let canvas = crate::warpaint::ground_rgb(crate::warpaint::Ground::Canvas);
            assert_ne!((ink, canvas), (own.fg, own.bg));
            let mut ui = Terminal::with_screen_size_and_cursor_position_for_test(
                CrosstermBackend::new(stdout()),
                screen,
                Position::ORIGIN,
            );
            ui.set_viewport_area(area);
            let drawn = captured(&mut ui, |ui| {
                ui.draw_with_size(screen, |frame| *frame.buffer_mut() = rule(area))
            })?;
            let [Operation::Bytes(invested), Operation::Frame(frame)] = drawn.as_slice() else {
                panic!("a drawn frame publishes the pack's colors, then reaches the writer");
            };
            assert_eq!(String::from_utf8_lossy(invested), published(ink, canvas));
            let mut bound = frame.buffer.clone();
            bound.set_string(
                area.x,
                area.y,
                "x",
                Style::default().fg(crate::warpaint::Ground::Canvas.color()),
            );
            let mut handoff = Vec::new();
            crate::warpaint::revoke(&mut handoff)?;
            assert_eq!(String::from_utf8_lossy(&handoff), published(own.fg, own.bg));
            let replayed = captured(&mut ui, |ui| ui.replay(&bound))?;
            let [Operation::Bytes(republished), Operation::Frame(frame)] = replayed.as_slice()
            else {
                panic!("a replay publishes the pack's colors, then hands the writer one frame");
            };
            assert_eq!(String::from_utf8_lossy(republished), published(ink, canvas));
            assert_eq!(frame.buffer, bound);
            Ok(())
        })
    }
}
