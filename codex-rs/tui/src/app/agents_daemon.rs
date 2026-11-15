//! Embedded sessions may start a shared daemon without transferring their own connection.
use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::AppServerTarget;
use super::Line;
use super::turn_submissions::ui_requests::UiRequestKind;
use super::turn_submissions::ui_requests::UiRequestScope;
use crate::bottom_pane::SelectionDescriptionLayout;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use ratatui::style::Stylize;

impl App {
    pub(super) fn request_agents_overview(&mut self, server: &AppServerSession) {
        if !matches!(self.app_server_target, AppServerTarget::Embedded) {
            self.open_agents_overview(server);
            return;
        }
        let workload_identity_selected = codex_login::is_workload_identity_selected();
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Shared agents unavailable".into()),
            subtitle: Some(
                if workload_identity_selected {
                    "The agents dashboard is unavailable while workload identity is active."
                } else {
                    "This session isn’t connected to a shared background server."
                }
                .into(),
            ),
            footer_note: (!workload_identity_selected).then(|| {
                Line::from(
                    "Starting a background server will not interrupt or move this session.".dim(),
                )
            }),
            items: [
                (!workload_identity_selected).then(|| SelectionItem {
                    name: "Start background server".into(),
                    description: Some("Open `codex agents` in another terminal afterward".into()),
                    actions: vec![Box::new(|tx| tx.send(AppEvent::StartAgentsDaemon))],
                    dismiss_on_select: true,
                    ..Default::default()
                }),
                Some(SelectionItem {
                    name: "Return to this session".into(),
                    dismiss_on_select: true,
                    ..Default::default()
                }),
            ]
            .into_iter()
            .flatten()
            .collect(),
            description_layout: SelectionDescriptionLayout::HideWhenNarrow {
                min_description_width: 28,
            },
            ..SelectionViewParams::picker()
        });
    }

    pub(super) fn start_agents_daemon(&mut self, server: &AppServerSession) {
        if !matches!(self.app_server_target, AppServerTarget::Embedded) {
            return;
        }
        self.request_server(
            server,
            UiRequestScope::Connection,
            UiRequestKind::Mutation,
            "Starting background server",
            |_| async { Ok(start_shared_daemon().await) },
            |app, _, result| {
                let result = match result {
                    Ok(result) => result,
                    Err(error) => Err(format!("Startup was not confirmed: {error}")),
                };
                app.app_event_tx
                    .send(AppEvent::AgentsDaemonStarted { result });
                Ok(())
            },
        );
    }
}

async fn start_shared_daemon() -> Result<(), String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let executable = if current.file_stem().and_then(std::ffi::OsStr::to_str) == Some("codex-tui") {
        current.with_file_name("codex")
    } else {
        current
    };
    // This handle owns only the foreground CLI. The daemon is separately supervised.
    let output = tokio::process::Command::new(executable)
        .args(["app-server", "daemon", "start"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if message.is_empty() {
            format!("daemon process exited with {}", output.status)
        } else {
            message
        })
    }
}
