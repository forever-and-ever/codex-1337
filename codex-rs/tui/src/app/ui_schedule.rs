//! Admission gates hold application, never terminal receipt. A held draft cannot answer a
//! startup modal first presented while it was held; that frame quarantines the held queue.
//! Draw demand advances timers independently of the completed-frame paint cadence.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;

use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseEventKind;
use futures::future::Either;
use tokio_stream::Stream;

use super::App;
use super::AppEvent;
use super::AppRunControl;
use super::AppServerSession;
use super::AppServerTarget;
use super::CODEX_CLI_VERSION;
use super::ExitMode;
use super::ExitReason;
use super::Path;
use super::RateLimitRefreshOrigin;
use super::Result;
use super::Size;
use super::THREAD_EVENT_CHANNEL_CAPACITY;
use super::ThreadBufferedEvent;
use super::TuiEvent;
use super::history_cell;
use super::mpsc;
use super::reconnect;
use super::select;
use super::tui;

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

    /// Polls with the UI task's own waker. Crossterm's reader thread latches the waker that armed
    /// it and ignores later polls until input arrives, so a noop-waker probe here would leave the
    /// task asleep while keys queue.
    pub(in crate::app) async fn drain_ready(
        &mut self,
        stream: &mut Pin<Box<dyn Stream<Item = TuiEvent> + Send>>,
    ) {
        if self.stream_closed {
            return;
        }
        // The snapshot is finite even when key repeat never stops producing input.
        std::future::poll_fn(|cx| {
            for _ in 0..INPUT_BATCH {
                let Poll::Ready(event) = stream.as_mut().poll_next(cx) else {
                    break;
                };
                let closed = event.is_none();
                self.receive(event);
                if closed {
                    break;
                }
            }
            Poll::Ready(())
        })
        .await;
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
    pub(super) fn complete_terminal_output(&mut self) {
        crate::custom_terminal::output::completion_received();
        self.transcript_view.promote_presented_frame();
        self.live_work.promote_presented_frame();
    }

    pub(crate) fn defer_input_frame(
        tui: &mut tui::Tui,
        event: &TuiEvent,
        screen_size: Size,
    ) -> bool {
        if matches!(event, TuiEvent::Draw) {
            let requester = tui.frame_requester();
            if crate::custom_terminal::output::paint_ready()
                && requester.paint_admitted()
                && tui.presentation_mode() != tui::PresentationMode::Frozen
            {
                return false;
            }
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

pub(in crate::app) fn has_pending_ui_events(
    app: &App,
    events: &mpsc::UnboundedReceiver<AppEvent>,
) -> bool {
    !events.is_empty() || app.saved_openings.has_ready()
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "Admission combines independent queue, draft, configuration, and header observations."
)]
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
    pub(in crate::app) fn request_modal_frame(&mut self, requester: &tui::FrameRequester) {
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
            tui.note_terminal_input(tui::Witness::Keyboard);
            self.apply_presentation_transition(tui, previous)?;
        }
        if (matches!(&event, TuiEvent::Key(key)
            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat))
            || matches!(&event, TuiEvent::Paste(_)))
            && !self.reconnect.offline
            && !pending_startup_draft
            && !waiting_for_initial_session_configured
            && !has_pending_ui_events(self, app_event_rx)
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

// Keep propagated failures distinct from controlled loop exits: the former bypass startup cleanup.
pub(super) async fn drive(
    app: &mut App,
    tui: &mut tui::Tui,
    backend: &mut super::backend_sessions::BackendSession,
    launch_choices: crate::app_server_session::StartupLaunchChoices,
    app_event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    tui_events: &mut Pin<Box<dyn Stream<Item = TuiEvent> + Send>>,
    pending_startup_draft: &mut Option<crate::bottom_pane::ComposerDraftSnapshot>,
    pending_startup_submission: &mut bool,
    schedule: &mut UiSchedule,
    output_completions: &mut Option<tokio::sync::broadcast::Receiver<()>>,
    #[cfg(target_os = "linux")] desktop_attention: &mut Option<
        tokio::sync::watch::Receiver<tui::DesktopAttention>,
    >,
    #[cfg(target_os = "linux")] mut ui_host_ready: bool,
    idle_restart: &mut crate::idle_restart::IdleRestart,
    mut waiting_for_initial_session_configured: bool,
    #[cfg(windows)] mut terminal_color_probe_pending: bool,
) -> Result<Result<ExitReason>> {
    use crate::ui_latency::Stage;
    use crate::ui_latency::enter;
    use tokio_stream::StreamExt;

    let mut last_screen_size = tui.terminal.last_known_screen_size;
    let mut settled_size_tags = crate::terminal_size_telemetry::tags(last_screen_size);
    let mut pending_resize: Option<(Size, Instant)> = None;
    let mut reconnect = None;
    let mut scheduled_owner = backend.owner();
    let mut waiting_for_initial_session_header = true;
    #[cfg(target_os = "linux")]
    let mut desktop_presence = crate::desktop_thread::DesktopPresence::new();
    #[cfg(target_os = "linux")]
    let mut desktop_route = None;
    let exit_reason_result = 'ui: loop {
        let displayed_owner = backend.owner();
        if scheduled_owner != displayed_owner {
            reconnect = None;
            scheduled_owner = displayed_owner;
            waiting_for_initial_session_header = false;
            waiting_for_initial_session_configured = app.pending_startup_thread_start;
        }
        if !backend.is_listening(displayed_owner)
            && !app.pending_startup_thread_start
            && !app.reconnect.offline
        {
            app.begin_reconnect();
        }
        if !app.reconnect.offline
            && let Some(event) = backend.take_ready_startup(|event| {
                !app.defer_side_startup_notification(event, displayed_owner)
            })
        {
            app.handle_polled_backend_event(backend.session(), displayed_owner, event)
                .await;
            schedule.event_applied();
        }
        schedule.drain_ready(tui_events).await;
        for _ in 0..INPUT_BATCH {
            let admitted = input_admitted(
                app,
                has_pending_ui_events(app, app_event_rx),
                pending_startup_draft.is_some(),
                waiting_for_initial_session_configured,
                waiting_for_initial_session_header,
            );
            let Some((stamp, event)) = schedule.next_input(admitted) else {
                break;
            };
            let _handler_stage = enter(Stage::TerminalEvent);
            if !matches!(event, TuiEvent::FocusLost) {
                idle_restart.postpone();
            }
            let interactive = is_interactive(&event);
            let result = app
                .apply_scheduled_input(
                    tui,
                    backend.session_mut(),
                    event,
                    pending_startup_draft.is_some(),
                    waiting_for_initial_session_configured,
                    app_event_rx,
                )
                .await;
            if let Some(stamp) = stamp {
                crate::ui_latency::input_handled(stamp);
            }
            if interactive {
                schedule.input_handled();
            }
            match result {
                Ok(AppRunControl::Continue) => {}
                Ok(AppRunControl::Exit(reason)) => break 'ui Ok(reason),
                Err(err) if app.recover_transport_error(&err) => {}
                Err(err) => break 'ui Err(err),
            }
        }
        if schedule.input_finished() {
            tracing::warn!("terminal input stream closed; shutting down active thread");
            break Ok(
                match app
                    .handle_exit_mode(backend.session_mut(), ExitMode::ShutdownFirst)
                    .await
                {
                    AppRunControl::Exit(reason) => reason,
                    AppRunControl::Continue => continue,
                },
            );
        }
        let paint_due = schedule.frame_due();
        if paint_due || schedule.take_maintenance() {
            let _handler_stage = enter(Stage::TerminalEvent);
            let protected = app.startup_pending_protected_request;
            let started_at = paint_due.then(|| schedule.frame_started());
            let requester = tui.frame_requester();
            requester.set_paint_admitted(paint_due);
            let result = app
                .handle_tui_event(tui, backend.session_mut(), TuiEvent::Draw)
                .await;
            requester.set_paint_admitted(true);
            if let Some(started_at) = started_at {
                schedule.frame_finished(started_at);
            }
            if protected && !app.startup_pending_protected_request {
                schedule.quarantine();
            }
            match result {
                Ok(AppRunControl::Continue) => {}
                Ok(AppRunControl::Exit(reason)) => break Ok(reason),
                Err(err) if app.recover_transport_error(&err) => {}
                Err(err) => break Err(err),
            }
            tokio::task::yield_now().await;
        }
        let maintenance_stage = enter(Stage::Maintenance);
        app.drive_server_operations(backend.session_mut());
        app.retry_pending_thread_delivery().await;
        // Reconnect can dismiss an overlay from the server-event path.
        if app.overlay.is_none() {
            if !tui.is_owned_screen() && tui.is_alt_screen_active() {
                app.close_transcript_overlay(tui);
            } else if let Err(err) = tui.set_mouse_capture(
                if tui.is_owned_screen()
                    && !app.chat_widget.has_active_modal()
                    && app.chat_widget.has_expanded_questions()
                {
                    tui::MouseCapture::Disabled
                } else {
                    tui::MouseCapture::Inherit
                },
            ) {
                break Err(err.into());
            }
        }
        if app.pending_open_resume_picker {
            app.pending_open_resume_picker = false;
            match Box::pin(app.open_resume_picker_for_backends(tui, backend.session_mut())).await {
                Ok(AppRunControl::Continue) => {}
                Ok(AppRunControl::Exit(reason)) => break Ok(reason),
                Err(err) if app.recover_transport_error(&err) => {}
                Err(err) => break Err(err),
            }
            continue;
        }
        if let Some(pending) = app.pending_working_directory_change.take() {
            Box::pin(app.finish_working_directory_change(tui, backend.session_mut(), pending))
                .await;
            continue;
        }
        if app.reconnect.offline && !app.reconnect.failed && reconnect.is_none() {
            let presentation = app.reconnect.presentation;
            let wait_for_drain =
                app.pending_startup_thread_start && backend.is_listening(displayed_owner);
            reconnect = Some(
                if let reconnect::ReconnectPresentation::Startup(deadline) = presentation
                    && wait_for_drain
                {
                    Either::Left(Box::pin(async move {
                        tokio::time::sleep_until(deadline).await;
                        color_eyre::eyre::bail!(
                            "Automatic reconnect timed out waiting for the server to finish shutting down."
                        );
                    }))
                } else {
                    Either::Right(Box::pin(reconnect::reconnect(
                        displayed_owner,
                        app.app_server_target.clone(),
                        app.config.clone(),
                        app.local_settings.clone(),
                        app.current_displayed_thread_id(),
                        backend
                            .session()
                            .remote_cwd_override()
                            .map(Path::to_path_buf),
                        presentation,
                    )))
                },
            );
        }
        // Replay queues history and operations. A buffered closure must not switch
        // widgets before those app events have been applied.
        let has_pending_app_events = has_pending_ui_events(app, app_event_rx);
        if !has_pending_app_events
            && !app.reconnect.offline
            && !app.thread_delivery.is_blocked()
            && let Some(replay_thread) = app.thread_delivery.primary_replay_thread
        {
            if !app.pending_primary_events.is_empty() {
                app.admit_next_primary_event().await?;
                continue;
            } else if app.chat_widget.thread_id() != Some(replay_thread) {
                app.thread_delivery.primary_replay_thread = None;
            } else if app
                .active_thread_rx
                .as_ref()
                .is_none_or(mpsc::Receiver::is_empty)
            {
                app.finish_primary_event_replay();
                continue;
            }
        }
        if !has_pending_app_events {
            if std::mem::take(&mut app.reload_requested) {
                if pending_startup_draft.is_some() {
                    app.chat_widget.add_error_message("Finish the startup dialog before reloading; your draft is still held there.".into());
                } else {
                    let _ = app.reload_ui(/*idle_candidate*/ None).await;
                }
            }
            #[cfg(target_os = "linux")]
            if !ui_host_ready && !waiting_for_initial_session_configured {
                tui.frame_requester().schedule_interactive_frame();
                crate::reload::worker::ready().await?;
                app.chat_widget
                    .set_queue_autosend_suppressed(/*suppressed*/ false);
                app.chat_widget.maybe_send_next_queued_input();
                ui_host_ready = true;
            }
        }
        #[cfg(target_os = "linux")]
        {
            let identity = (displayed_owner, app.reconnect.connection_epoch);
            if desktop_route
                .as_ref()
                .is_none_or(|(known, _)| *known != identity)
            {
                desktop_route = Some((
                    identity,
                    crate::desktop_thread::DesktopRoute::from_target(
                        &app.app_server_target,
                        backend.session().uses_embedded_app_server(),
                        app.config.codex_home.as_path(),
                    ),
                ));
            }
            if let Some((_, route)) = desktop_route.as_ref() {
                let displayed_thread = (!app.reconnect.offline
                    && !app.pending_startup_thread_start
                    && app.pending_thread_switch_resets == 0)
                    .then(|| app.current_displayed_thread_id())
                    .flatten();
                desktop_presence.show(route, displayed_thread);
            }
        }
        idle_restart.set_ready(pending_startup_draft.is_none() && app.ready_for_idle_restart());
        let block_terminal_input_for_pending_startup_events = !input_admitted(
            app,
            has_pending_app_events,
            pending_startup_draft.is_some(),
            waiting_for_initial_session_configured,
            waiting_for_initial_session_header,
        );
        let pause_events = (app.reconnect.offline && !app.pending_startup_thread_start)
            || app.thread_delivery.is_blocked()
            || app.thread_delivery.primary_replay_thread.is_some()
            || !(app.primary_thread_id.is_none()
                && app.pending_primary_events.len() < THREAD_EVENT_CHANNEL_CAPACITY
                || app.primary_thread_id.is_some() && app.pending_primary_events.is_empty())
            || (!matches!(app.app_server_target, AppServerTarget::Embedded)
                && has_pending_app_events);
        let size = tui.terminal.last_known_screen_size;
        if !crate::terminal_size_telemetry::enabled(&app.config)
            || app.terminal_size_tags.is_empty()
        {
            pending_resize = None;
            settled_size_tags = crate::terminal_size_telemetry::tags(size);
        } else if size != last_screen_size {
            pending_resize = Some((size, Instant::now() + Duration::from_secs(15)));
        }
        last_screen_size = size;
        let rate_limit_poll_deadline = app
            .chat_widget
            .rate_limit_refresh_interval()
            .and_then(|interval| app.rate_limit_refresh_state.poll_deadline(interval));
        crate::ui_latency::admission(
            if app.pending_thread_switch_resets != 0 {
                crate::ui_latency::InputGate::ThreadSwitch
            } else if !app.reconnect.offline && app.thread_delivery.primary_replay_thread.is_some()
            {
                crate::ui_latency::InputGate::Replay
            } else if !app.reconnect.offline && block_terminal_input_for_pending_startup_events {
                crate::ui_latency::InputGate::Startup
            } else {
                crate::ui_latency::InputGate::Open
            },
            app_event_rx.len(),
            app.active_thread_rx.as_ref().map_or(0, mpsc::Receiver::len),
            app.pending_primary_events.len(),
        );
        drop(maintenance_stage);
        let select_stage = enter(Stage::IdleSelect);
        let frame_wake = schedule.wake_at();
        let control = select! {
            () = crate::custom_terminal::output::next_completion(output_completions) => {
                drop(select_stage);
                app.complete_terminal_output();
                AppRunControl::Continue
            }
            () = async {
                match frame_wake {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending().await,
                }
            } => {
                drop(select_stage);
                schedule.frame_woken();
                AppRunControl::Continue
            }
            result = async {
                #[cfg(target_os = "linux")]
                {
                match desktop_attention.as_mut() {
                    Some(receiver) => receiver.changed().await,
                    None => std::future::pending().await,
                }
                }
                #[cfg(not(target_os = "linux"))]
                std::future::pending::<std::result::Result<(), tokio::sync::watch::error::RecvError>>().await
            } => {
                drop(select_stage);
                let _handler_stage = enter(Stage::Timer);
                #[cfg(target_os = "linux")]
                {
                let previous = tui.presentation_mode();
                if result.is_ok() && let Some(receiver) = desktop_attention.as_mut() {
                    let state = *receiver.borrow_and_update();
                    tui.set_desktop_attention(state);
                } else {
                    *desktop_attention = None;
                    tui.set_desktop_attention(tui::DesktopAttention::default());
                }
                app.apply_presentation_transition(tui, previous)?;
                AppRunControl::Continue
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = result;
                    AppRunControl::Continue
                }
            }
            Some(event) = app.saved_openings.next_event(app_event_rx) => {
                drop(select_stage);
                let _handler_stage = enter(Stage::AppEvent);
                let is_initial_session_header = matches!(
                    &event,
                    AppEvent::InsertHistoryCell(cell)
                        if cell.as_any().is::<history_cell::SessionInfoCell>()
                );
                let had_active_modal = app.chat_widget.has_active_modal();
                match Box::pin(app.handle_event(tui, backend.session_mut(), event)).await {
                    Ok(AppRunControl::Continue) => {
                        if is_initial_session_header {
                            waiting_for_initial_session_header = false;
                        }
                        if !had_active_modal && app.chat_widget.has_active_modal() {
                            app.request_modal_frame(&tui.frame_requester());
                        }
                        AppRunControl::Continue
                    }
                    Ok(AppRunControl::Exit(reason)) => AppRunControl::Exit(reason),
                    Err(err) if app.recover_transport_error(&err) => AppRunControl::Continue,
                    Err(err) => break Err(err),
                }
            }
            active = async {
                if let Some(rx) = app.active_thread_rx.as_mut() {
                    rx.recv().await
                } else {
                    None
                }
            }, if App::should_handle_active_thread_events(
                waiting_for_initial_session_configured,
                app.active_thread_rx.is_some()
            ) && !has_pending_app_events && !app.reconnect.offline => {
                drop(select_stage);
                let _handler_stage = enter(Stage::ThreadEvent);
                if let Some(event) = active {
                    if let Err(err) = app.handle_active_thread_event(tui, backend.session_mut(), event).await {
                        break Err(err);
                    }
                } else {
                    app.clear_active_thread().await;
                }
                AppRunControl::Continue
            }
            event = tui_events.next(), if !schedule.stream_closed() => {
                drop(select_stage);
                let _handler_stage = enter(Stage::TerminalEvent);
                schedule.receive(event);
                AppRunControl::Continue
            }
            Some(completion) = app.server_operations.control_rx.recv() => {
                drop(select_stage);
                let _handler_stage = enter(Stage::ServerCompletion);
                match Box::pin(app.handle_server_completion(tui, backend.session_mut(), completion)).await {
                    Ok(control) => control,
                    Err(err) if app.recover_transport_error(&err) => AppRunControl::Continue,
                    Err(err) => break Err(err),
                }
            }
            completion = app.thread_delivery.completion_rx.recv(), if !app.thread_delivery.is_blocked()
                && app.thread_delivery.primary_replay_thread.is_none() => {
                drop(select_stage);
                let _handler_stage = enter(Stage::DeliveryCompletion);
                if let Some(completion) = completion {
                    match app.handle_local_thread_completion(backend.session_mut(), completion).await {
                        Ok(()) => {},
                        Err(err) if app.recover_transport_error(&err) => {},
                        Err(err) => break Err(err),
                    }
                }
                AppRunControl::Continue
            }
            (owner, event) = backend.next_event(pause_events) => {
                drop(select_stage);
                let _handler_stage = enter(Stage::ServerEvent);
                if let Some(event) = event {
                        if owner == displayed_owner
                            && app.reconnect.offline
                            && app.pending_startup_thread_start
                            && matches!(&event.event, codex_app_server_client::AppServerEvent::Disconnected { .. })
                        {
                            reconnect = None;
                        }
                        if app.defer_side_startup_notification(&event, displayed_owner) {
                            backend.defer_startup(event);
                        } else {
                        app.handle_polled_backend_event(
                            backend.session(),
                            displayed_owner,
                            event,
                        ).await;
                        }
                } else {
                        if owner == displayed_owner {
                            if app.reconnect.offline && app.pending_startup_thread_start {
                                reconnect = None;
                            } else if !app.pending_startup_thread_start {
                                app.begin_reconnect();
                            }
                        }
                        tracing::warn!("app-server event stream closed");
                }
                AppRunControl::Continue
            }
            result = async { match reconnect.as_mut() { Some(future) => future.await, None => std::future::pending().await } }, if reconnect.is_some() && !has_pending_app_events => {
                drop(select_stage);
                let _handler_stage = enter(Stage::Reconnect);
                reconnect = None;
                match result {
                    Ok(connected) => {
                        app.finish_reconnect(tui, backend.session_mut(), app_event_rx, connected, CODEX_CLI_VERSION).await?;
                        backend.resume_events(displayed_owner);
                        waiting_for_initial_session_configured = app.pending_startup_thread_start;
                        if app.pending_startup_thread_start {
                            super::spawn_startup_thread_start(
                                backend.session(),
                                app.local_settings.clone(),
                                app.config.clone(),
                                launch_choices.clone(),
                                app.app_event_tx.clone(),
                                app.harness_overrides.model_provider.clone(),
                            );
                        }
                    }
                    Err(error) => {
                        if app.pending_startup_thread_start {
                            app.recover_failed_startup_reconnect();
                        }
                        app.reconnect.failed = true;
                        app.chat_widget.reconnect_failed();
                        app.chat_widget.add_error_message(error.to_string());
                        if let Ok(mut state) = app.agents_overview.view_state.lock() {
                            state.connection_notice = Some("Reconnect failed — agent list is stale; relaunch to retry");
                        }
                    }
                }
                AppRunControl::Continue
            }
            () = async {
                match pending_resize {
                    Some((_, deadline)) => tokio::time::sleep_until(deadline.into()).await,
                    None => std::future::pending().await,
                }
            } => {
                drop(select_stage);
                let _handler_stage = enter(Stage::Timer);
                if let Some((expected, _)) = pending_resize.take()
                    && crate::terminal_size_telemetry::enabled(&app.config)
                {
                    let actual = crate::terminal_size_telemetry::read_size();
                    if actual == expected {
                        let tags = crate::terminal_size_telemetry::tags(actual);
                        if tags != settled_size_tags {
                            crate::terminal_size_telemetry::record(
                                &app.terminal_size_tags, &app.config, "resize", actual,
                            );
                        }
                        settled_size_tags = tags;
                    } else {
                        pending_resize = Some((actual, Instant::now() + Duration::from_secs(15)));
                    }
                }
                AppRunControl::Continue
            }
            () = async {
                match rate_limit_poll_deadline {
                    Some(deadline) => {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    }
                    None => std::future::pending().await,
                }
            }, if backend.is_listening(displayed_owner) => {
                drop(select_stage);
                let _handler_stage = enter(Stage::Timer);
                app.refresh_rate_limits(backend.session_mut(), RateLimitRefreshOrigin::Periodic);
                AppRunControl::Continue
            }
            () = async {
                match app.chat_widget.terminal_title_next_refresh {
                    Some(deadline) => {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    }
                    None => std::future::pending().await,
                }
            }, if tui.presentation_mode() == tui::PresentationMode::Normal => {
                drop(select_stage);
                let _handler_stage = enter(Stage::Timer);
                app.chat_widget.refresh_goal_status_indicator_for_time_tick();
                app.chat_widget.refresh_terminal_title();
                AppRunControl::Continue
            }
            () = async {
                match app.commit_animation.as_mut() {
                    Some(interval) => {
                        interval.tick().await;
                    }
                    None => std::future::pending().await,
                }
            }, if !has_pending_app_events
                && tui.presentation_mode() == tui::PresentationMode::Normal => {
                drop(select_stage);
                let _handler_stage = enter(Stage::Timer);
                crate::session_log::log_commit_tick();
                app.chat_widget.on_commit_tick();
                AppRunControl::Continue
            }
            () = idle_restart.tick(), if !has_pending_app_events => {
                drop(select_stage);
                let _handler_stage = enter(Stage::Timer);
                if !has_pending_ui_events(app, app_event_rx) {
                    app.try_idle_restart(tui, idle_restart).await;
                }
                AppRunControl::Continue
            }
        };
        schedule.event_applied();
        let _maintenance_stage = enter(Stage::Maintenance);
        if App::should_stop_waiting_for_initial_session(
            waiting_for_initial_session_configured,
            app.primary_thread_id,
        ) {
            waiting_for_initial_session_configured = false;
            let had_active_modal = app.chat_widget.has_active_modal();
            if let Err(err) = app.drain_active_thread_events(tui).await {
                break Err(err);
            }
            if !had_active_modal && app.chat_widget.has_active_modal() {
                app.request_modal_frame(&tui.frame_requester());
            }
        }
        match control {
            AppRunControl::Continue => {
                if app.reconnect.offline && !app.pending_startup_thread_start {
                    *pending_startup_submission = false;
                    app.chat_widget.cancel_startup_submission();
                }
                if !has_pending_ui_events(app, app_event_rx)
                    && !app.has_queued_startup_protected_request()
                {
                    app.chat_widget.restore_startup_input_when_ready(
                        pending_startup_draft,
                        pending_startup_submission,
                    );
                }
                #[cfg(windows)]
                if terminal_color_probe_pending
                    && app.ready_for_terminal_color_probe(has_pending_ui_events(app, app_event_rx))
                {
                    tui.probe_default_colors_after_protected_startup();
                    terminal_color_probe_pending = false;
                }
            }
            AppRunControl::Exit(reason) => break Ok(reason),
        }
    };
    Ok(exit_reason_result)
}
