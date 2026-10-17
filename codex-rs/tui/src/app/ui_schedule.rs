//! Admission gates hold application, never terminal receipt. A held draft cannot answer a
//! startup modal first presented while it was held; that frame quarantines the held queue.
//! Draw demand advances timers independently of the completed-frame paint cadence.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;
use std::time::Instant;

use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseEventKind;
use futures::FutureExt;
use tokio_stream::Stream;
use tokio_stream::StreamExt;

use super::*;

pub(in crate::app) const INPUT_BATCH: usize = 64;
const EVENT_BATCH: usize = 64;
const EVENT_BUDGET: Duration = Duration::from_millis(4);

#[derive(Clone, Copy)]
enum Urgency {
    Resume,
    Interactive,
    Passive,
}

struct FrameTiming {
    started_at: Instant,
    ended_at: Instant,
}

#[derive(Clone, Copy)]
enum FrameDemand {
    None,
    Awaiting(Urgency),
    Maintenance(Urgency),
    Paint(Urgency),
    Ready(Urgency),
}

impl FrameDemand {
    fn urgency(self) -> Option<Urgency> {
        match self {
            Self::None => None,
            Self::Awaiting(urgency)
            | Self::Maintenance(urgency)
            | Self::Paint(urgency)
            | Self::Ready(urgency) => Some(urgency),
        }
    }
}

pub(in crate::app) struct UiSchedule {
    input: VecDeque<(Option<crate::ui_latency::InputStamp>, TuiEvent)>,
    frame: FrameDemand,
    last_frame: Option<FrameTiming>,
    events_started: Instant,
    events: usize,
    stream_closed: bool,
}

impl UiSchedule {
    pub(in crate::app) fn new() -> Self {
        let now = Instant::now();
        Self {
            input: VecDeque::new(),
            frame: FrameDemand::None,
            last_frame: None,
            events_started: now,
            events: 0,
            stream_closed: false,
        }
    }

    pub(in crate::app) fn receive(&mut self, event: Option<TuiEvent>) {
        match event {
            Some(TuiEvent::Draw) => {
                if matches!(self.frame, FrameDemand::None | FrameDemand::Awaiting(_)) {
                    self.events_started = Instant::now();
                    self.events = 0;
                }
                let urgency = self.frame.urgency().unwrap_or(Urgency::Passive);
                self.frame = if matches!(self.frame, FrameDemand::Ready(_)) {
                    FrameDemand::Ready(urgency)
                } else {
                    FrameDemand::Maintenance(urgency)
                };
            }
            Some(event) => {
                let stamp = crate::ui_latency::input_consumed(&event);
                if matches!(event, TuiEvent::Resume) {
                    self.frame = FrameDemand::Awaiting(Urgency::Resume);
                }
                if matches!(&event, TuiEvent::Mouse(mouse) if mouse.kind == MouseEventKind::Moved)
                    && matches!(self.input.back(), Some((_, TuiEvent::Mouse(mouse))) if mouse.kind == MouseEventKind::Moved)
                {
                    self.input.pop_back();
                }
                self.input.push_back((stamp, event));
            }
            None => self.stream_closed = true,
        }
    }

    pub(in crate::app) fn drain_ready(
        &mut self,
        stream: &mut Pin<Box<dyn Stream<Item = TuiEvent> + Send>>,
    ) {
        if self.stream_closed {
            return;
        }
        // The snapshot is finite even when key repeat never stops producing input.
        for _ in 0..INPUT_BATCH {
            let Some(event) = stream.next().now_or_never() else {
                break;
            };
            let closed = event.is_none();
            self.receive(event);
            if closed {
                break;
            }
        }
    }

    pub(in crate::app) fn next_input(
        &mut self,
        admitted: bool,
    ) -> Option<(Option<crate::ui_latency::InputStamp>, TuiEvent)> {
        if let Some(index) = self
            .input
            .iter()
            .position(|(_, event)| matches!(event, TuiEvent::Resume))
        {
            return self.input.remove(index);
        }
        if admitted {
            self.input.pop_front()
        } else {
            // An interrupt cannot be trapped behind replay or a thread-switch reset.
            let index = self.input.iter().position(|(_, event)| {
                matches!(event, TuiEvent::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c' | 'C')))
            })?;
            if matches!(self.input.get(index), Some((_, TuiEvent::Key(_))))
                && self
                    .input
                    .iter()
                    .take(index)
                    .any(|(_, event)| matches!(event, TuiEvent::Key(_) | TuiEvent::Paste(_)))
            {
                // Ctrl+C first clears typeahead; it must not leapfrog it into an interrupt.
                self.input.drain(..=index);
                return None;
            }
            self.input.remove(index)
        }
    }

    pub(in crate::app) fn quarantine(&mut self) {
        self.input.clear();
    }

    pub(in crate::app) fn stream_closed(&self) -> bool {
        self.stream_closed
    }

    pub(in crate::app) fn input_finished(&self) -> bool {
        self.stream_closed && self.input.is_empty()
    }

    pub(in crate::app) fn event_applied(&mut self) {
        self.events = self.events.saturating_add(1);
    }

    pub(in crate::app) fn take_maintenance(&mut self) -> bool {
        if let FrameDemand::Maintenance(urgency) = self.frame {
            self.frame = FrameDemand::Paint(urgency);
            true
        } else {
            false
        }
    }

    pub(in crate::app) fn frame_due(&self) -> bool {
        !matches!(self.frame, FrameDemand::None | FrameDemand::Awaiting(_))
            && Instant::now() >= self.paint_at()
            && (matches!(self.frame, FrameDemand::Ready(_))
                || self.events >= EVENT_BATCH
                || Instant::now().duration_since(self.events_started) >= EVENT_BUDGET)
    }

    fn paint_at(&self) -> Instant {
        match (&self.last_frame, self.frame.urgency()) {
            (_, Some(Urgency::Resume)) | (None, _) => self.events_started,
            (Some(frame), urgency) => tui::FrameRequester::next_frame_at(
                frame.started_at,
                frame.ended_at,
                matches!(urgency, Some(Urgency::Interactive)),
            ),
        }
    }

    pub(in crate::app) fn frame_woken(&mut self) {
        if let Some(urgency) = self.frame.urgency() {
            self.frame = FrameDemand::Ready(urgency);
        }
    }

    pub(in crate::app) fn input_handled(&mut self) {
        let urgency = if matches!(self.frame.urgency(), Some(Urgency::Resume)) {
            Urgency::Resume
        } else {
            Urgency::Interactive
        };
        self.frame = match self.frame {
            FrameDemand::Maintenance(_) => FrameDemand::Maintenance(urgency),
            FrameDemand::Ready(_) => FrameDemand::Ready(urgency),
            FrameDemand::Paint(_) => FrameDemand::Paint(urgency),
            FrameDemand::None | FrameDemand::Awaiting(_) => FrameDemand::Awaiting(urgency),
        };
    }

    pub(in crate::app) fn wake_at(&self) -> Option<Instant> {
        match self.frame {
            FrameDemand::None | FrameDemand::Awaiting(_) => None,
            _ => Some(self.paint_at()),
        }
    }

    pub(in crate::app) fn frame_started(&mut self) -> Instant {
        self.frame = FrameDemand::None;
        Instant::now()
    }

    pub(in crate::app) fn frame_finished(&mut self, started_at: Instant) {
        let ended_at = Instant::now();
        self.last_frame = Some(FrameTiming {
            started_at,
            ended_at,
        });
        self.events_started = ended_at;
        self.events = 0;
    }
}

impl App {
    pub(crate) fn defer_input_frame(
        tui: &mut tui::Tui,
        event: &TuiEvent,
        screen_size: ratatui::layout::Size,
    ) -> bool {
        if matches!(event, TuiEvent::Draw) {
            let requester = tui.frame_requester();
            if crate::custom_terminal::output::paint_ready()
                && requester.paint_admitted()
                && tui.presentation_mode() != tui::PresentationMode::Frozen
            {
                return false;
            }
            // Reduced presentation consumed dirty before reaching this seam. A maintenance
            // wake cannot acknowledge content which has not actually been presented.
            requester.retain_dirty();
            tui.defer_screen_size(screen_size);
            tui.clipboard.advance(requester);
            return true;
        }
        tui.defer_screen_size(screen_size);
        if matches!(event, TuiEvent::Resize(_) | TuiEvent::Resume) {
            tui.terminal.invalidate_viewport();
        }
        tui.frame_requester().schedule_interactive_frame();
        true
    }
}

pub(in crate::app) fn is_interactive(event: &TuiEvent) -> bool {
    matches!(event, TuiEvent::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat))
        || matches!(event, TuiEvent::Paste(_))
        || matches!(event, TuiEvent::Mouse(mouse) if mouse.kind != MouseEventKind::Moved)
}

pub(in crate::app) fn input_admitted(
    app: &App,
    has_pending_app_events: bool,
    pending_startup_draft: bool,
    waiting_for_initial_session_configured: bool,
    waiting_for_initial_session_header: bool,
) -> bool {
    let initial_session_header_pending = waiting_for_initial_session_header
        && app.primary_session_configured.is_some()
        && has_pending_app_events;
    app.pending_thread_switch_resets == 0
        && (app.reconnect.offline
            || (!(app.startup_pending_protected_request && app.chat_widget.has_active_modal())
                && app.thread_delivery.primary_replay_thread.is_none()
                && !initial_session_header_pending
                && (!(pending_startup_draft || app.startup_protected_input_boundary)
                    || !has_pending_app_events)
                && (waiting_for_initial_session_configured
                    || !app.has_queued_startup_protected_request())))
}

impl App {
    pub(in crate::app) fn request_modal_frame(&mut self, requester: tui::FrameRequester) {
        if self.startup_protected_input_boundary && self.chat_widget.has_active_modal() {
            self.startup_pending_protected_request = true;
        }
        requester.schedule_interactive_frame();
    }

    pub(in crate::app) async fn apply_scheduled_input(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: TuiEvent,
        pending_startup_draft: bool,
        waiting_for_initial_session_configured: bool,
        app_event_rx: &mpsc::UnboundedReceiver<AppEvent>,
    ) -> Result<AppRunControl> {
        if matches!(&event, TuiEvent::Resume) {
            let previous = tui.presentation_mode();
            tui.note_terminal_input();
            self.apply_presentation_transition(tui, previous)?;
        }
        if (matches!(&event, TuiEvent::Key(key)
            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat))
            || matches!(&event, TuiEvent::Paste(_)))
            && !self.reconnect.offline
            && !pending_startup_draft
            && !waiting_for_initial_session_configured
            && app_event_rx.is_empty()
            && !self.startup_pending_protected_request
            && self
                .active_thread_rx
                .as_ref()
                .is_none_or(mpsc::Receiver::is_empty)
            && !self
                .pending_primary_events
                .iter()
                .any(|event| matches!(event, ThreadBufferedEvent::Request(_)))
        {
            self.startup_protected_input_boundary = false;
        }
        self.handle_tui_event(tui, app_server, event).await
    }
}
