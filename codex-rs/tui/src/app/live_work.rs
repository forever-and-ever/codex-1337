//! The selected conversation's live projection and owned process inventory.
//! Requests are scoped to an attachment and generation; late replies cannot
//! populate another conversation. Opening this view never loads an agent.

use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::ClientRequest;
use super::Duration;
use super::HashMap;
use super::HashSet;
use super::Instant;
use super::KeyEvent;
use super::KeyEventKind;
use super::Rect;
use super::Result;
use super::ThreadId;
use super::TuiEvent;
use super::history_cell;
use super::tui;
use crate::app_server_session::session_preparation::RequestPolicy;
use crate::app_server_session::session_preparation::{self as preparation};
use crate::live_work::Action;
use crate::live_work::Group;
use crate::live_work::Interaction;
use crate::live_work::Key;
use crate::live_work::LiveWorkView;
use crate::live_work::Row;
use crate::transcript_view::ViewAction;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::text::Line;
use uuid::Uuid;

const REFRESH: Duration = Duration::from_secs(2);
const RETRY: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct LiveWorkState {
    scope: Option<Scope>,
}

struct Scope {
    thread: ThreadId,
    view: LiveWorkView,
    work: Vec<Row>,
    processes: Processes,
    stopping: HashMap<(ThreadId, String), Uuid>,
    show_empty_processes: bool,
}

enum Processes {
    Unloaded,
    Fetching {
        request: Uuid,
        previous: Vec<api::ThreadBackgroundTerminal>,
    },
    Ready {
        items: Vec<api::ThreadBackgroundTerminal>,
        refresh_at: Instant,
    },
    Unavailable {
        error: String,
        retry_at: Instant,
    },
}

#[derive(Debug)]
pub(crate) enum Event {
    FocusProcesses,
    Refresh {
        thread: ThreadId,
        request: Uuid,
    },
    Refreshed {
        thread: ThreadId,
        request: Uuid,
        result: Result<Vec<api::ThreadBackgroundTerminal>, String>,
    },
    Action(Action),
    Stopped {
        thread: ThreadId,
        owner: ThreadId,
        process_id: String,
        request: Uuid,
        result: Result<(), String>,
    },
}

fn text(value: impl Into<String>) -> Line<'static> {
    history_cell::sanitize_user_text(value.into().into())
        .into_owned()
        .into()
}

fn row(key: Key, summary: Line<'static>) -> Row {
    Row {
        key,
        summary,
        details: Vec::new(),
        children: Vec::new(),
        actions: Vec::new(),
    }
}

fn group(group: Group, children: Vec<Row>) -> Row {
    Row {
        key: Key::Group(group),
        summary: Line::default(),
        details: Vec::new(),
        children,
        actions: Vec::new(),
    }
}

impl Processes {
    fn items(&self) -> &[api::ThreadBackgroundTerminal] {
        match self {
            Self::Ready { items, .. } => items,
            Self::Fetching { previous, .. } => previous,
            Self::Unloaded | Self::Unavailable { .. } => &[],
        }
    }

    fn refresh(&mut self, now: Instant) -> Option<Uuid> {
        match self {
            Self::Fetching { .. } => return None,
            Self::Ready { refresh_at, .. } if now < *refresh_at => return None,
            Self::Unavailable { retry_at, .. } if now < *retry_at => return None,
            _ => {}
        }
        let request = Uuid::new_v4();
        let old = std::mem::replace(
            self,
            Self::Fetching {
                request,
                previous: Vec::new(),
            },
        );
        if let Self::Ready { items, .. } = old {
            *self = Self::Fetching {
                request,
                previous: items,
            };
        }
        Some(request)
    }

    fn refresh_in(&self) -> Option<Duration> {
        match self {
            Self::Ready { refresh_at, .. } => {
                Some(refresh_at.saturating_duration_since(Instant::now()))
            }
            Self::Unavailable { retry_at, .. } => {
                Some(retry_at.saturating_duration_since(Instant::now()))
            }
            Self::Unloaded | Self::Fetching { .. } => None,
        }
    }
}

impl LiveWorkState {
    pub(super) fn is_focused(&self) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|scope| scope.view.is_focused())
    }
    pub(super) fn desired_height(&self, width: u16) -> u16 {
        self.scope
            .as_ref()
            .map_or(0, |scope| scope.view.desired_height(width))
    }

    pub(super) fn blur(&mut self) {
        if let Some(scope) = &mut self.scope {
            scope.view.blur();
        }
    }

    pub(super) fn has_selection(&self) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|scope| scope.view.has_selection())
    }

    pub(super) fn owns_selection_key(&self, key: KeyEvent) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|scope| scope.view.owns_selection_key(key))
    }

    pub(super) fn cancel_primary(&mut self) {
        if let Some(scope) = &mut self.scope {
            scope.view.selection_view().cancel_primary();
        }
    }

    pub(super) fn finish_copy(
        &mut self,
        completion: &(u64, crate::clipboard_copy::worker::CopyResult),
        current: bool,
    ) {
        if let Some(scope) = &mut self.scope {
            let view = scope.view.selection_view();
            let completed = view.finish_copy(&[], completion, current);
            if completed.is_some()
                && completion.1 == Ok(crate::clipboard_copy::CopyStatus::Confirmed)
                && !view.has_selection_range()
            {
                view.jump_to_latest();
            }
        }
    }

    pub(super) fn render(&mut self, area: Rect, buffer: &mut Buffer) {
        if let Some(scope) = &mut self.scope {
            scope.view.render(area, buffer);
        }
    }

    pub(super) fn capture_painted_frame(&mut self) {
        if let Some(scope) = &mut self.scope {
            scope.view.capture_painted_frame();
        }
    }

    pub(super) fn promote_presented_frame(&mut self) {
        if let Some(scope) = &mut self.scope {
            scope.view.promote_presented_frame();
        }
    }

    pub(super) fn refresh_in(&self) -> Option<Duration> {
        self.scope
            .as_ref()
            .and_then(|scope| scope.processes.refresh_in())
    }

    fn accept_inventory(
        &mut self,
        thread: ThreadId,
        request: Uuid,
        result: Result<Vec<api::ThreadBackgroundTerminal>, String>,
    ) {
        let Some(scope) = self.scope.as_mut().filter(|scope| scope.thread == thread) else {
            return;
        };
        if !matches!(scope.processes, Processes::Fetching { request: id, .. } if id == request) {
            return;
        }
        scope.processes = match result {
            Ok(items) => {
                scope.stopping.retain(|(owner, process_id), _| {
                    items.iter().any(|item| {
                        item.thread_id == owner.to_string() && &item.process_id == process_id
                    })
                });
                Processes::Ready {
                    items,
                    refresh_at: Instant::now() + REFRESH,
                }
            }
            Err(error) => Processes::Unavailable {
                error,
                retry_at: Instant::now() + RETRY,
            },
        };
    }

    #[expect(
        clippy::expect_used,
        reason = "The requested scope is installed immediately before it is returned."
    )]
    fn scope(&mut self, thread: ThreadId) -> &mut Scope {
        if self
            .scope
            .as_ref()
            .is_none_or(|scope| scope.thread != thread)
        {
            self.scope = Some(Scope {
                thread,
                view: LiveWorkView::default(),
                work: Vec::new(),
                processes: Processes::Unloaded,
                stopping: HashMap::new(),
                show_empty_processes: false,
            });
        }
        self.scope.as_mut().expect("selected live-work scope")
    }
}

impl App {
    pub(super) fn retire_work_connection(&mut self) {
        self.reconnect.connection_epoch.advance();
        self.live_work.scope = None;
    }

    #[expect(
        clippy::expect_used,
        reason = "Process admission validates each process owner as a ThreadId."
    )]
    pub(super) fn prepare_live_work(&mut self) {
        let Some(thread) = self.active_thread_id.filter(|_| !self.reconnect.offline) else {
            self.live_work.scope = None;
            return;
        };
        let work = self
            .thread_event_channels
            .get(&thread)
            .map_or(Some(Vec::new()), |channel| {
                channel.store.try_lock().ok().map(|store| {
                    if store.connection_epoch == self.reconnect.connection_epoch {
                        store.operations.rows(thread)
                    } else {
                        Vec::new()
                    }
                })
            });
        if !self.chat_widget.no_modal_or_popup_active()
            || self.chat_widget.is_external_writer_view()
            || self.overlay.is_some()
        {
            self.live_work.blur();
        }
        let alarms = self.chat_widget.live_alarm_rows();
        let owners = self.chat_widget.live_process_owners();
        let scope = self.live_work.scope(thread);
        // The event pump briefly holds this lock; paint the last observation instead of blocking.
        if let Some(work) = work {
            scope.work = work;
        }
        if let Some(request) = scope.processes.refresh(Instant::now()) {
            self.app_event_tx
                .send(AppEvent::LiveWork(Event::Refresh { thread, request }));
        }
        let mut processes = Vec::new();
        for process in scope.processes.items() {
            let owner = ThreadId::from_string(&process.thread_id).expect("validated process owner");
            let owner_name = if owner == thread {
                "this agent".to_owned()
            } else {
                owners
                    .get(&owner)
                    .cloned()
                    .unwrap_or_else(|| owner.to_string())
            };
            let stopping = scope
                .stopping
                .contains_key(&(owner, process.process_id.clone()));
            let mut item = row(
                Key::Process {
                    thread_id: owner,
                    id: process.process_id.clone(),
                },
                crate::render::header::leading(
                    if stopping { "Stopping" } else { "Running" },
                    text(format!(
                        " · {owner_name} · {}",
                        process.command.lines().next().unwrap_or_default()
                    )),
                ),
            );
            item.details = process.command.lines().map(text).collect();
            item.details.extend([
                text(format!("Owner: {owner} · session: {}", process.process_id)),
                text(format!("Directory: {}", process.cwd)),
            ]);
            if owner == thread {
                item.actions.push(Action::RevealCall {
                    thread_id: owner,
                    item_id: process.item_id.clone(),
                });
            }
            if !stopping {
                item.actions.push(Action::TerminateProcess {
                    thread_id: owner,
                    process_id: process.process_id.clone(),
                });
            }
            processes.push(item);
        }
        match &scope.processes {
            Processes::Unloaded | Processes::Fetching { .. }
                if processes.is_empty() && scope.show_empty_processes =>
            {
                processes.push(row(Key::ProcessStatus, text("Loading processes…")));
            }
            Processes::Unavailable { error, .. } => {
                let mut status = row(Key::ProcessStatus, text("Process inventory unavailable"));
                status.details.push(text(error.clone()));
                processes.push(status);
            }
            Processes::Ready { items, .. } if items.is_empty() && scope.show_empty_processes => {
                processes.push(row(Key::ProcessStatus, text("No running processes")));
            }
            _ => {}
        }
        scope.view.set_rows(vec![
            group(Group::Work, scope.work.clone()),
            group(Group::Processes, processes),
            group(Group::Alarms, alarms),
        ]);
    }

    pub(super) fn handle_live_work_selection(
        &mut self,
        tui: &mut tui::Tui,
        event: &TuiEvent,
    ) -> bool {
        if !self.chat_widget.no_modal_or_popup_active()
            || self.chat_widget.is_external_writer_view()
        {
            return false;
        }
        let Some(scope) = &mut self.live_work.scope else {
            return false;
        };
        scope.view.promote_presented_frame();
        scope.view.selection_view().copy_on_select = self.transcript_view.copy_on_select;
        scope
            .view
            .selection_view()
            .set_keymap_bindings(&self.keymap);
        let Some(action) = scope.view.selection_event(event) else {
            return false;
        };

        let view = scope.view.selection_view();
        if let Some((characters, result)) = view.copy_action(
            tui,
            &[],
            &action,
            crate::clipboard_copy::CopyFormat::PlainText,
        ) {
            if result == Ok(crate::clipboard_copy::CopyStatus::Confirmed) {
                view.jump_to_latest();
            }
            self.transcript_view.show_copy_feedback(&result, characters);
        } else if matches!(
            action,
            ViewAction::OpenLink(_) | ViewAction::OpenThreadLink(_)
        ) {
            unreachable!("Work selection never activates links");
        }
        tui.frame_requester().schedule_frame();
        true
    }

    pub(super) fn handle_live_work_key_event(
        &mut self,
        tui: &mut tui::Tui,
        event: &TuiEvent,
    ) -> bool {
        if matches!(event, TuiEvent::Key(key) if key.kind != KeyEventKind::Release)
            && (self.handle_live_work_selection(tui, event) || self.handle_live_work_input(event))
        {
            tui.frame_requester().schedule_frame();
            return true;
        }
        false
    }

    pub(super) fn handle_live_work_input(&mut self, event: &TuiEvent) -> bool {
        if matches!(event, TuiEvent::FocusLost | TuiEvent::Resume) {
            if let Some(scope) = &mut self.live_work.scope {
                scope.view.blur();
            }
            return false;
        }
        if !self.chat_widget.no_modal_or_popup_active()
            || self.chat_widget.is_external_writer_view()
        {
            if let Some(scope) = &mut self.live_work.scope {
                scope.view.blur();
            }
            return false;
        }
        let Some(scope) = &mut self.live_work.scope else {
            return false;
        };
        scope.view.promote_presented_frame();
        let interaction = match event {
            TuiEvent::Mouse(mouse) => scope.view.handle_mouse(*mouse),
            TuiEvent::Key(key) => scope.view.handle_key(*key),
            _ => return false,
        };
        match interaction {
            Interaction::Ignored => {
                if matches!(event, TuiEvent::Key(key) if key.kind != KeyEventKind::Release)
                    || matches!(event, TuiEvent::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::Down(_)))
                {
                    scope.view.blur();
                }
                false
            }
            Interaction::Consumed => true,
            Interaction::Action(action) => {
                self.app_event_tx
                    .send(AppEvent::LiveWork(Event::Action(action)));
                true
            }
        }
    }

    pub(super) fn handle_live_work_event(&mut self, server: &AppServerSession, event: Event) {
        match event {
            Event::FocusProcesses => {
                self.prepare_live_work();
                if let Some(scope) = &mut self.live_work.scope {
                    scope.show_empty_processes = true;
                }
                self.prepare_live_work();
                if let Some(scope) = &mut self.live_work.scope {
                    scope.view.focus_group(Group::Processes);
                }
            }
            Event::Refresh { thread, request } => {
                if !self.live_work.scope.as_ref().is_some_and(|scope|
                    scope.thread == thread && matches!(scope.processes, Processes::Fetching { request: id, .. } if id == request)
                ) { return; }
                let handle = server.request_handle();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let result = process_inventory(&handle, thread, request).await;
                    tx.send(AppEvent::LiveWork(Event::Refreshed {
                        thread,
                        request,
                        result,
                    }));
                });
            }
            Event::Refreshed {
                thread,
                request,
                result,
            } => {
                self.live_work.accept_inventory(thread, request, result);
            }
            Event::Action(Action::RevealCall { thread_id, item_id }) => {
                if self.active_thread_id != Some(thread_id) {
                    return;
                }
                if self
                    .transcript_view
                    .reveal_call(&self.transcript_cells, &item_id)
                {
                    if let Some(scope) = &mut self.live_work.scope {
                        scope.view.blur();
                    }
                } else {
                    self.chat_widget.add_info_message(
                        "The original call is outside the loaded history.".into(),
                        None,
                    );
                }
            }
            Event::Action(Action::DisarmAlarm { thread_id, id }) => {
                if self.active_thread_id == Some(thread_id) {
                    self.app_event_tx
                        .send(AppEvent::DisarmAlarm { thread_id, id });
                }
            }
            Event::Action(Action::TerminateProcess {
                thread_id: owner,
                process_id,
            }) => {
                let Some(scope) = &mut self.live_work.scope else {
                    return;
                };
                let key = (owner, process_id.clone());
                if scope.stopping.contains_key(&key)
                    || !scope.processes.items().iter().any(|item| {
                        item.thread_id == owner.to_string() && item.process_id == process_id
                    })
                {
                    return;
                }
                let request = Uuid::new_v4();
                scope.stopping.insert(key, request);
                let thread = scope.thread;
                let handle = server.request_handle();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let result: Result<api::ThreadBackgroundTerminalsTerminateResponse, _> =
                        preparation::request(
                            RequestPolicy::Interactive,
                            &handle,
                            ClientRequest::ThreadBackgroundTerminalsTerminate {
                                request_id: api::RequestId::String(format!("live-stop-{request}")),
                                params: api::ThreadBackgroundTerminalsTerminateParams {
                                    thread_id: owner.to_string(),
                                    process_id: process_id.clone(),
                                },
                            },
                        )
                        .await;
                    tx.send(AppEvent::LiveWork(Event::Stopped {
                        thread,
                        owner,
                        process_id,
                        request,
                        result: result.map(|_| ()).map_err(|error| error.to_string()),
                    }));
                });
            }
            Event::Stopped {
                thread,
                owner,
                process_id,
                request,
                result,
            } => {
                let Some(scope) = self
                    .live_work
                    .scope
                    .as_mut()
                    .filter(|scope| scope.thread == thread)
                else {
                    return;
                };
                let key = (owner, process_id);
                if scope.stopping.get(&key) != Some(&request) {
                    return;
                }
                if let Err(error) = result {
                    scope.stopping.remove(&key);
                    self.chat_widget
                        .add_error_message(format!("Could not stop process: {error}"));
                }
                if let Processes::Ready { refresh_at, .. } = &mut scope.processes {
                    *refresh_at = Instant::now();
                }
            }
        }
    }
}

async fn process_inventory(
    handle: &AppServerRequestHandle,
    thread: ThreadId,
    request: Uuid,
) -> Result<Vec<api::ThreadBackgroundTerminal>, String> {
    let mut cursor = None;
    let mut cursors = HashSet::new();
    let mut items = Vec::new();
    loop {
        let response: api::ThreadBackgroundTerminalsListResponse = preparation::request(
            RequestPolicy::Interactive,
            handle,
            ClientRequest::ThreadBackgroundTerminalsList {
                request_id: api::RequestId::String(format!(
                    "live-processes-{request}-{}",
                    items.len()
                )),
                params: api::ThreadBackgroundTerminalsListParams {
                    thread_id: thread.to_string(),
                    cursor,
                    limit: Some(100),
                    scope: api::ThreadBackgroundTerminalsScope::Subtree,
                },
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        for item in &response.data {
            ThreadId::from_string(&item.thread_id)
                .map_err(|error| format!("Invalid process owner: {error}"))?;
        }
        items.extend(response.data);
        cursor = response.next_cursor;
        match &cursor {
            None => return Ok(items),
            Some(cursor) if !cursors.insert(cursor.clone()) => {
                return Err("Process inventory repeated a page cursor.".into());
            }
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support;
    use crossterm::event::KeyCode;
    use ratatui::layout::Size;

    #[tokio::test]
    async fn process_replies_and_controls_follow_attachment_not_screen_position() {
        // A previous attachment can answer after switching away and back to the same thread.
        let mut app = Box::pin(test_support::make_test_app()).await;
        let first = ThreadId::new();
        let second = ThreadId::new();
        app.active_thread_id = Some(first);
        app.prepare_live_work();
        let Processes::Fetching {
            request: old_request,
            ..
        } = app.live_work.scope.as_ref().unwrap().processes
        else {
            panic!("inventory requested")
        };
        app.active_thread_id = Some(second);
        app.prepare_live_work();
        app.active_thread_id = Some(first);
        app.prepare_live_work();
        let Processes::Fetching { request, .. } = app.live_work.scope.as_ref().unwrap().processes
        else {
            panic!("new inventory requested")
        };
        app.live_work
            .accept_inventory(first, old_request, Err("stale".into()));
        assert!(matches!(
            app.live_work.scope.as_ref().unwrap().processes,
            Processes::Fetching { request: active, .. } if active == request
        ));
        let process = api::ThreadBackgroundTerminal {
            thread_id: second.to_string(),
            item_id: "call".into(),
            process_id: "42".into(),
            command: "owned worker".into(),
            cwd: app.config.cwd.clone().into(),
            os_pid: None,
            cpu_percent: None,
            rss_kb: None,
        };
        app.live_work
            .accept_inventory(first, request, Ok(vec![process]));
        app.prepare_live_work();
        let scope = app.live_work.scope.as_mut().unwrap();
        scope.view.focus_group(Group::Processes);
        scope.view.handle_key(KeyCode::Down.into());
        scope.view.handle_key(KeyCode::Right.into());
        scope.view.handle_key(KeyCode::Down.into());
        assert_eq!(
            scope.view.handle_key(KeyCode::Enter.into()),
            Interaction::Action(Action::TerminateProcess {
                thread_id: second,
                process_id: "42".into()
            })
        );
        // Ordinary typing leaves the live control rather than turning the next space into a click.
        assert!(!app.handle_live_work_input(&TuiEvent::Key(KeyCode::Char('x').into())));
        assert!(!app.live_work.scope.as_ref().unwrap().view.is_focused());
        assert!(app.transcript_cells.is_empty());
        let mut tui = tui::test_support::make_test_tui().unwrap();
        tui.set_owned_screen(true).unwrap();
        let size = Size::new(80, 24);
        tui.terminal.resize(size).unwrap();
        app.render_owned_transcript(&mut tui, size).unwrap();
        let buffer = crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal);
        let lines = buffer
            .content()
            .chunks(80)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let rule = lines
            .iter()
            .position(|line| line.contains("────────"))
            .unwrap();
        let processes = lines
            .iter()
            .position(|line| line.contains("Processes"))
            .unwrap();
        assert!(processes > rule);
        assert!(lines.iter().any(|line| line.contains("Stop process")));
        tui.set_owned_screen(false).unwrap();
        app.reconnect.offline = true;
        app.prepare_live_work();
        assert!(app.live_work.scope.is_none());
    }
}
