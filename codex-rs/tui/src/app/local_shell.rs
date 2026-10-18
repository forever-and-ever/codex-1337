use super::App;
use crate::app_event::AppEvent;
use crate::local_shell::LocalShellAdmission;
use crate::local_shell::LocalShellCommand;
use crate::local_shell::LocalShellRequest;
use crate::local_shell::LocalShellResult;
use crate::local_shell::LocalShellSubmission;
use crate::local_shell::LocalShellUpdate;
use crate::pager_overlay::Overlay;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use std::path::PathBuf;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

// TELOS(client-only-shell): Visible local intent owns one draft lease and never becomes agent input.

pub(crate) struct LocalShell {
    cwd: Result<PathBuf, String>,
    state: State,
    admission: LocalShellAdmission,
}

enum State {
    Idle,
    Running(Job),
    Stopping(Job),
    Stalled(Job),
    Finished(LocalShellResult),
    Rejected {
        submission: LocalShellSubmission,
        error: String,
    },
}

struct Job {
    command: LocalShellCommand,
    cancellation: CancellationToken,
    task: AbortOnDropHandle<()>,
    _lease: OwnedSemaphorePermit,
}

impl State {
    fn job_mut(&mut self) -> Option<&mut Job> {
        match self {
            Self::Running(job) | Self::Stopping(job) | Self::Stalled(job) => Some(job),
            Self::Idle | Self::Finished(_) | Self::Rejected { .. } => None,
        }
    }

    fn request_stop(&mut self) -> bool {
        *self = std::mem::replace(self, Self::Idle).into_stopping();
        matches!(self, Self::Stopping(_) | Self::Stalled(_))
    }

    fn into_stopping(self) -> Self {
        match self {
            Self::Running(job) => {
                job.cancellation.cancel();
                Self::Stopping(job)
            }
            state => state,
        }
    }
}

impl LocalShell {
    pub(crate) fn capture() -> Self {
        Self {
            cwd: std::env::current_dir().map_err(|error| format!("Host cwd unavailable: {error}")),
            state: State::Idle,
            admission: LocalShellAdmission::default(),
        }
    }

    pub(super) fn is_running(&self) -> bool {
        matches!(
            self.state,
            State::Running(_) | State::Stopping(_) | State::Stalled(_)
        )
    }

    pub(super) fn blocks_reload(&self) -> bool {
        self.admission.is_claimed()
    }

    fn notice(&self) -> Option<String> {
        Some(match &self.state {
            State::Idle => return None,
            State::Rejected { .. } => {
                "Local shell not started; command retained · !! restore/view".to_owned()
            }
            State::Stalled(_) => {
                "Local shell did not stop; still waiting for exit · !! view".to_owned()
            }
            State::Stopping(_) => "Local shell stopping · !! view".to_owned(),
            State::Running(_) => "Local shell running · Ctrl-C stop · !! view".to_owned(),
            State::Finished(result) => match &result.outcome {
                Err(_) => "Local shell failed · !! view".to_owned(),
                Ok(code) if result.cancelled => {
                    format!("Local shell stopped · exit {code} · !! view")
                }
                Ok(code) => format!("Local shell exit {code} · !! view"),
            },
        })
    }

    pub(super) async fn shutdown(&mut self) -> Option<String> {
        let _ = self.state.request_stop();
        if let Some(Job { task, .. }) = self.state.job_mut() {
            // Abort-on-drop still owns cleanup if the normal exit wait cannot finish.
            if tokio::time::timeout(std::time::Duration::from_secs(3), &mut *task)
                .await
                .is_err()
            {
                tracing::warn!("local shell shutdown exit wait timed out");
                task.abort();
                let _ = (&mut *task).await;
            }
        }
        // Disclose only commands that never reached execute; a possibly executed command
        // is not retryable recovery. Taking the state releases the owned lease.
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Rejected { submission, error } => Some(format!(
                "Client-only local command was not started ({error}); retained command at exit (escaped):\n!! {}",
                submission.command.text().escape_debug()
            )),
            _ => None,
        }
    }
}

impl Default for LocalShell {
    fn default() -> Self {
        Self::capture()
    }
}

impl App {
    pub(super) fn consume_local_shell_event(
        &mut self,
        event: AppEvent,
        tui: &mut Tui,
    ) -> Option<AppEvent> {
        match event {
            AppEvent::LocalShellRequest(request) => self.handle_local_shell_request(request, tui),
            AppEvent::LocalShellUpdate(update) => self.handle_local_shell_update(update, tui),
            event => return Some(event),
        }
        None
    }

    pub(super) fn handle_local_shell_request(&mut self, request: LocalShellRequest, tui: &mut Tui) {
        match request {
            LocalShellRequest::Show => {
                if !self.restore_rejected_local_shell(tui) {
                    self.show_local_shell(tui);
                }
            }
            LocalShellRequest::Run(submission) => self.start_local_shell(submission, tui),
        }
    }

    pub(super) fn reject_local_shell_submission(
        &mut self,
        submission: LocalShellSubmission,
        error: String,
        tui: &mut Tui,
    ) {
        self.local_shell.state = State::Rejected {
            submission,
            error: error.clone(),
        };
        self.chat_widget.flash_local_shell_error(error);
        self.sync_local_shell_notice();
        tui.frame_requester().schedule_frame();
    }

    fn restore_rejected_local_shell(&mut self, tui: &mut Tui) -> bool {
        let State::Rejected { submission, .. } = &self.local_shell.state else {
            return false;
        };
        if !self
            .chat_widget
            .restore_local_shell_command(submission.command.text())
        {
            return false;
        }
        self.local_shell.state = State::Idle;
        self.chat_widget
            .flash_local_shell_error("Local command restored, not run".to_owned());
        self.sync_local_shell_notice();
        tui.frame_requester().schedule_frame();
        true
    }

    pub(super) async fn shutdown_local_shell(
        &mut self,
        pending_events: &mut UnboundedReceiver<AppEvent>,
    ) -> Option<String> {
        // At most one unstarted run can exist: it still owns the same admission lease.
        while let Ok(event) = pending_events.try_recv() {
            if let AppEvent::LocalShellRequest(LocalShellRequest::Run(submission)) = event {
                debug_assert!(!self.local_shell.is_running());
                self.local_shell.state = State::Rejected {
                    submission,
                    error: "TUI closed before dispatch".to_owned(),
                };
            }
        }
        self.local_shell.shutdown().await
    }

    fn start_local_shell(&mut self, submission: LocalShellSubmission, tui: &mut Tui) {
        // The request owns the sole admission from before the composer was cleared.
        debug_assert!(!self.local_shell.is_running());
        let cwd = match &self.local_shell.cwd {
            Ok(cwd) => cwd.clone(),
            Err(error) => {
                let error = error.clone();
                self.reject_local_shell_submission(submission, error, tui);
                return;
            }
        };
        let LocalShellSubmission { command, lease } = submission;
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker_command = command.clone();
        let tx = self.app_event_tx.clone();
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            crate::local_shell::execute(worker_command, cwd, worker_cancellation, |update| {
                tx.send(AppEvent::LocalShellUpdate(update));
            })
            .await;
        }));
        self.local_shell.state = State::Running(Job {
            command,
            cancellation,
            task,
            _lease: lease,
        });
        self.sync_local_shell_notice();
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn handle_local_shell_update(&mut self, update: LocalShellUpdate, tui: &mut Tui) {
        match update {
            LocalShellUpdate::Stalled => {
                self.local_shell.state =
                    match std::mem::replace(&mut self.local_shell.state, State::Idle) {
                        State::Running(job) | State::Stopping(job) => State::Stalled(job),
                        state => state,
                    };
            }
            LocalShellUpdate::Finished(result) => {
                self.local_shell.state = State::Finished(result);
            }
        }
        self.sync_local_shell_notice();
        tui.frame_requester().schedule_frame();
    }

    pub(super) fn sync_local_shell_notice(&mut self) {
        self.chat_widget.set_local_shell_context(
            self.local_shell.admission.clone(),
            self.local_shell.notice(),
        );
    }

    pub(super) fn cancel_local_shell_key(&mut self, event: &TuiEvent, tui: &mut Tui) -> bool {
        let TuiEvent::Key(key) = event else {
            return false;
        };
        if self.overlay.is_some()
            || !self.chat_widget.no_modal_or_popup_active()
            || key.kind != KeyEventKind::Press
            || !key.modifiers.contains(KeyModifiers::CONTROL)
            || !matches!(key.code, KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'c'))
        {
            return false;
        }
        if !self.local_shell.state.request_stop() {
            return false;
        }
        self.sync_local_shell_notice();
        tui.frame_requester().schedule_frame();
        true
    }

    fn show_local_shell(&mut self, tui: &mut Tui) {
        use crate::history_cell::output_provenance::OutputMode;
        use crate::history_cell::output_provenance::client_lines;
        use crate::history_cell::output_provenance::client_notice;

        let cwd = match &self.local_shell.state {
            State::Finished(result) => result.cwd.display().to_string(),
            _ => match &self.local_shell.cwd {
                Ok(cwd) => cwd.display().to_string(),
                Err(error) => error.clone(),
            },
        };
        let mut lines = vec![client_notice("Client-only · Local shell").line];
        lines.extend(
            client_lines(&format!("TUI host cwd: {cwd}"), OutputMode::Raw)
                .into_iter()
                .map(|line| line.line),
        );
        let mut append = |text: &str| {
            lines.extend(
                client_lines(text, OutputMode::Raw)
                    .into_iter()
                    .map(|line| line.line),
            );
        };
        match &self.local_shell.state {
            State::Idle => append(
                "No local command has run. Use !! command; commands run on this TUI host, not the agent host.",
            ),
            State::Running(Job { command, .. })
            | State::Stopping(Job { command, .. })
            | State::Stalled(Job { command, .. }) => {
                append(&format!("!! {}", command.text()));
                if let Some(notice) = self.local_shell.notice() {
                    append(&notice);
                }
                append("Status snapshot; close and use !! to refresh.");
                append("Close this viewer, then Ctrl-C to stop the local process group.");
            }
            State::Rejected { submission, error } => {
                append(&format!("!! {}", submission.command.text()));
                append(&format!("Not started: {error}"));
                append("Command retained here, not queued for execution.");
                append(
                    "Close this viewer, clear the composer, then use !! to restore it without running.",
                );
            }
            State::Finished(result) => {
                append(&format!("!! {}", result.command.text()));
                let outcome = match &result.outcome {
                    Ok(code) if result.cancelled => format!("Stopped · exit {code}"),
                    Ok(code) => format!("Exit {code}"),
                    Err(error) => format!("Failed: {error}"),
                };
                append(&outcome);
                append(&String::from_utf8_lossy(&result.output));
                if result.discarded > 0 {
                    append(&format!(
                        "Output truncated; {} bytes discarded",
                        result.discarded
                    ));
                }
            }
        }
        self.overlay = Some(Overlay::new_static_with_lines(
            lines,
            "Local shell · Client-only".to_owned(),
            self.keymap.pager.clone(),
        ));
        tui.frame_requester().schedule_frame();
    }
}
