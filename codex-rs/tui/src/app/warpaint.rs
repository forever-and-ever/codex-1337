//! TELOS(warpaint): one resolver follows the active conversation's canonical family.

use super::App;
use super::turn_submissions::ui_requests::UiRequestKind;
use super::turn_submissions::ui_requests::UiRequestScope;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::typed_requests::SessionRpc;
use crate::render::highlight;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol as api;
use codex_protocol::ThreadId;
use std::collections::HashSet;
use uuid::Uuid;

pub(super) struct FamilyTheme {
    root: String,
    pack: Option<String>,
}

#[derive(Default)]
pub(super) struct ThemeState {
    pub(super) request: Option<Uuid>,
    family: Option<(ThreadId, FamilyTheme)>,
}

async fn read_family(
    handle: &AppServerRequestHandle,
    thread: ThreadId,
) -> Result<FamilyTheme, TypedRequestError> {
    let mut id = thread.to_string();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(id.clone()) {
            return Err(TypedRequestError::Server {
                method: "thread/read".into(),
                source: api::JSONRPCErrorError {
                    code: -32603,
                    message: "cyclic thread parentage".into(),
                    data: None,
                },
            });
        }
        let response = handle
            .thread_read(
                api::RequestId::String(Uuid::new_v4().to_string()),
                api::ThreadReadParams {
                    thread_id: id,
                    include_turns: false,
                },
            )
            .await?;
        if let Some(parent) = response.thread.parent_thread_id {
            id = parent;
        } else {
            return Ok(FamilyTheme {
                root: response.thread.id,
                pack: response.thread.theme,
            });
        }
    }
}

impl App {
    fn theme_thread(&self) -> Option<ThreadId> {
        if self
            .chat_widget
            .selected_index_for_present_view(super::AGENTS_OVERVIEW_VIEW_ID)
            .is_some()
        {
            None
        } else {
            self.active_thread_id
        }
    }

    pub(super) fn preview_config_theme(&self) {
        let family = self
            .theme
            .family
            .as_ref()
            .filter(|(thread, _)| self.theme_thread() == Some(*thread));
        let pack = crate::warpaint::resolved_pack(
            self.local_settings.tui.theme.as_deref(),
            family.map(|(_, family)| (family.root.as_str(), None)),
        );
        highlight::set_syntax_theme(
            crate::warpaint::theme(pack).unwrap_or_else(crate::warpaint::default_theme),
        );
        self.app_event_tx.send(AppEvent::SyntaxThemePreviewed);
    }

    pub(super) fn resolve_runtime_theme(&self) {
        if self
            .chat_widget
            .active_view_is(crate::theme_picker::THEME_PICKER_VIEW_ID)
        {
            return;
        }
        let family = self
            .theme
            .family
            .as_ref()
            .filter(|(thread, _)| self.theme_thread() == Some(*thread));
        let pack = crate::warpaint::resolved_pack(
            self.local_settings.tui.theme.as_deref(),
            family.map(|(_, family)| (family.root.as_str(), family.pack.as_deref())),
        );
        if highlight::current_syntax_theme().name.as_deref() != Some(pack) {
            highlight::set_syntax_theme(
                crate::warpaint::theme(pack).unwrap_or_else(crate::warpaint::default_theme),
            );
            self.app_event_tx.send(AppEvent::SyntaxThemePreviewed);
        }
    }

    pub(super) fn refresh_family_theme(&mut self, server: &AppServerSession) {
        let request = Uuid::new_v4();
        self.theme.request = Some(request);
        self.resolve_runtime_theme();
        let Some(thread) = self.active_thread_id else {
            self.theme.family = None;
            self.theme.request = None;
            return;
        };
        self.request_server(
            server,
            UiRequestScope::Thread(thread),
            UiRequestKind::ThreadMetadataRead,
            "Loading conversation theme",
            move |handle| async move { read_family(&handle, thread).await },
            move |app, _, result| {
                if app.theme.request != Some(request) || app.active_thread_id != Some(thread) {
                    return Ok(());
                }
                app.theme.request = None;
                match result {
                    Ok(family) => {
                        app.theme.family = Some((thread, family));
                        app.resolve_runtime_theme();
                    }
                    Err(error) => app
                        .chat_widget
                        .add_error_message(format!("Failed to load conversation theme: {error}")),
                }
                Ok(())
            },
        );
    }

    pub(super) fn save_family_theme(&mut self, server: &AppServerSession, pack: Option<String>) {
        let Some(thread) = self.theme_thread() else {
            self.resolve_runtime_theme();
            self.chat_widget
                .add_error_message("Open a conversation to save its theme.".into());
            return;
        };
        if let Some(name) = pack.as_deref()
            && crate::warpaint::theme(name).is_none()
        {
            self.chat_widget
                .add_error_message(format!("Unknown theme: {name}"));
            self.resolve_runtime_theme();
            return;
        }
        let request = Uuid::new_v4();
        self.theme.request = Some(request);
        self.request_server(
            server,
            UiRequestScope::Connection,
            UiRequestKind::Mutation,
            "Saving conversation theme",
            move |handle| async move {
                let mut family = read_family(&handle, thread).await?;
                let response = handle
                    .thread_metadata_update(
                        api::RequestId::String(request.to_string()),
                        api::ThreadMetadataUpdateParams {
                            thread_id: family.root.clone(),
                            project_id: None,
                            git_info: None,
                            daybreak_enabled: None,
                            comms: None,
                            theme: Some(pack),
                        },
                    )
                    .await?;
                family.pack = response.thread.theme;
                Ok(family)
            },
            move |app, _, result| {
                match result {
                    Ok(family)
                        if app.active_thread_id == Some(thread)
                            && app.theme.request == Some(request) =>
                    {
                        app.theme.family = Some((thread, family));
                    }
                    Ok(_) => {}
                    Err(error) => app
                        .chat_widget
                        .add_error_message(format!("Failed to save conversation theme: {error}")),
                }
                if app.theme.request == Some(request) {
                    app.theme.request = None;
                }
                app.resolve_runtime_theme();
                Ok(())
            },
        );
    }
}
