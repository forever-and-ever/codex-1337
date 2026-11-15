use super::AppEvent;
use super::Arc;
use super::ChatWidget;
use super::CustomPromptView;
use super::Duration;
use super::Instant;
use super::KeyCode;
use super::ModelPreset;
use super::PlainHistoryCell;
use super::SelectionItem;
use super::SelectionViewParams;
use super::ThreadId;
use super::key_hint;
use crate::openrouter::ACCOUNT_SELECTION_VIEW;
use crate::openrouter::Event;
use crate::openrouter::ModelMenu;
use crate::openrouter::Request;
use crate::openrouter::StatusFailure;
use crate::openrouter::StatusRefresh;
use crate::openrouter::VIEW;
use codex_app_server_protocol::OpenRouterKeyState;
use codex_app_server_protocol::OpenRouterKeyStatus;
use codex_app_server_protocol::OpenRouterModelEndpointsResponse;
use codex_protocol::KeyName;
use codex_protocol::ModelSlug;

mod views;

#[derive(Default)]
pub(super) struct OpenRouterUi {
    request: Option<uuid::Uuid>,
    status_request: Option<uuid::Uuid>,
    last_refresh: Option<Instant>,
    status: Option<OpenRouterKeyStatus>,
}

impl ChatWidget {
    pub(super) fn on_openrouter(&self) -> bool {
        self.config.model_provider_id == codex_model_provider_info::OPENROUTER_PROVIDER_ID
    }

    pub(super) fn open_openrouter_model_menu(&mut self) -> bool {
        if !self.on_openrouter() {
            return false;
        }
        let selected = match self.current_model().parse::<ModelSlug>() {
            Ok(slug) => slug,
            Err(error) => {
                self.show_openrouter_error(error);
                return true;
            }
        };
        self.app_event_tx
            .send(AppEvent::OpenRouter(Event::Request(Request::Models {
                menu: ModelMenu::Current,
                selected: Some(selected),
            })));
        true
    }

    pub(crate) fn start_openrouter_request(&mut self, id: uuid::Uuid, request: &Request) -> bool {
        if self.openrouter_request_pending() {
            return false;
        }
        self.openrouter_ui.request = Some(id);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(crate::openrouter::BUSY_VIEW),
            title: Some(request.title()),
            items: vec![SelectionItem {
                name: "Loading…".into(),
                is_disabled: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        });
        true
    }

    pub(crate) fn finish_openrouter_request(&mut self, id: uuid::Uuid) -> bool {
        if self.openrouter_ui.request != Some(id) {
            return false;
        }
        self.openrouter_ui.request = None;
        let visible = self.bottom_pane.active_view_id() == Some(crate::openrouter::BUSY_VIEW);
        self.bottom_pane
            .dismiss_view_by_id(crate::openrouter::BUSY_VIEW);
        visible
    }

    pub(crate) fn show_openrouter_models(
        &mut self,
        menu: ModelMenu,
        listing: crate::model_catalog::ModelListing,
        selected: Option<&ModelSlug>,
        error: Option<String>,
    ) {
        self.bottom_pane.dismiss_view_by_id(VIEW);
        self.bottom_pane
            .dismiss_view_by_id(crate::openrouter::ENDPOINTS_VIEW);
        if matches!(menu, ModelMenu::Current) && error.is_none() {
            let catalog = Arc::make_mut(&mut self.model_catalog);
            catalog.models.clone_from(&listing.models);
            catalog.providers.clone_from(&listing.providers);
            self.refresh_effective_service_tier();
            self.refresh_model_dependent_surfaces();
        }
        let params = self.openrouter_model_params(&menu, &listing.models, selected, error);
        let view = views::ModelView::new(
            params,
            listing.models,
            menu,
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    fn openrouter_model_params(
        &self,
        menu: &ModelMenu,
        models: &[ModelPreset],
        selected: Option<&ModelSlug>,
        error: Option<String>,
    ) -> SelectionViewParams {
        let mut items = models
            .iter()
            .map(|model| {
                let action_model = model.clone();
                let menu = menu.clone();
                let effort_picker = matches!(menu, ModelMenu::Current)
                    && model.supported_reasoning_efforts.len() > 1;
                SelectionItem {
                    name: model.display_name.clone(),
                    description: Some(model.description.clone()),
                    is_current: matches!(menu, ModelMenu::Current)
                        && self.current_model() == model.model,
                    search_value: Some(format!("{} {}", model.model, model.display_name)),
                    actions: vec![Box::new(move |tx| match &menu {
                        ModelMenu::Current => tx.send(AppEvent::OpenReasoningPopup {
                            model: action_model.clone(),
                        }),
                        ModelMenu::New(provider) => tx.send(AppEvent::OpenProviderReasoning {
                            provider: provider.clone(),
                            model: action_model.clone(),
                        }),
                    })],
                    dismiss_on_select: !effort_picker,
                    dismiss_parent_on_child_accept: effort_picker,
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();
        let add_menu = menu.clone();
        items.push(SelectionItem {
            name: "Add model…".into(),
            search_value: Some("Add model".into()),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenRouter(Event::AddModel {
                    menu: add_menu.clone(),
                }));
            })],
            dismiss_on_select: false,
            ..Default::default()
        });
        if matches!(menu, ModelMenu::Current) {
            // Model-only hotkeys address the leading model rows, never provider navigation.
            items.extend(self.provider_choices().map(|(_, choice)| choice));
        }
        SelectionViewParams {
            view_id: Some(VIEW),
            title: Some(menu.title().into()),
            subtitle: error,
            initial_selected_idx: selected.and_then(|selected| {
                models
                    .iter()
                    .position(|model| model.model == selected.as_str())
            }),
            items,
            is_searchable: true,
            footer_hint: Some(crate::openrouter::MODEL_FOOTER.into()),
            ..SelectionViewParams::picker()
        }
    }

    pub(crate) fn openrouter_model_prompt(
        &mut self,
        menu: ModelMenu,
        initial: String,
        error: Option<String>,
    ) {
        let tx = self.app_event_tx.clone();
        self.bottom_pane.show_text_prompt(CustomPromptView::new(
            "Add model…".into(),
            "moonshotai/kimi-k3 or @preset/name".into(),
            initial,
            error,
            Box::new(move |input| {
                tx.send(AppEvent::OpenRouter(Event::ModelSubmitted {
                    menu: menu.clone(),
                    input,
                }));
            }),
        ));
    }

    pub(crate) fn show_openrouter_endpoints(
        &mut self,
        menu: ModelMenu,
        slug: ModelSlug,
        response: OpenRouterModelEndpointsResponse,
    ) {
        self.bottom_pane
            .dismiss_view_by_id(crate::openrouter::ENDPOINTS_VIEW);
        self.bottom_pane
            .show_view(Box::new(views::EndpointsView::new(
                menu,
                slug,
                response,
                self.app_event_tx.clone(),
                self.bottom_pane.list_keymap(),
            )));
    }

    pub(crate) fn show_openrouter_error(&mut self, error: String) {
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("OpenRouter".into()),
            subtitle: Some(error),
            items: vec![SelectionItem {
                name: "Back".into(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        });
    }

    pub(crate) fn show_openrouter_failure(&mut self, request: Request, error: String) {
        match request {
            Request::Save { menu, slug } => {
                self.openrouter_model_prompt(menu, slug.to_string(), Some(error));
            }
            Request::AddKey {
                thread_id,
                name,
                path,
            } => self.openrouter_key_path_prompt(
                thread_id,
                name,
                path.to_string_lossy().into_owned(),
                Some(error),
            ),
            Request::Models { menu, .. } => self.show_openrouter_models(
                menu,
                crate::model_catalog::ModelListing {
                    models: Vec::new(),
                    providers: Vec::new(),
                },
                None,
                Some(error),
            ),
            request => {
                let tx_request = request.clone();
                self.bottom_pane.show_selection_view(SelectionViewParams {
                    view_id: Some(VIEW),
                    title: Some(request.title()),
                    subtitle: Some(error),
                    items: vec![SelectionItem {
                        name: "Retry".into(),
                        actions: vec![Box::new(move |tx| {
                            tx.send(AppEvent::OpenRouter(Event::Request(tx_request.clone())));
                        })],
                        dismiss_on_select: true,
                        ..Default::default()
                    }],
                    ..SelectionViewParams::picker()
                });
            }
        }
    }

    pub(crate) fn openrouter_key_name_prompt(
        &mut self,
        thread_id: ThreadId,
        initial: String,
        error: Option<String>,
    ) {
        let tx = self.app_event_tx.clone();
        self.bottom_pane.show_text_prompt(CustomPromptView::new(
            "Add OpenRouter key…".into(),
            "Key name".into(),
            initial,
            error,
            Box::new(move |input| {
                tx.send(AppEvent::OpenRouter(Event::KeyNameSubmitted {
                    thread_id,
                    input,
                }));
            }),
        ));
    }

    pub(crate) fn openrouter_key_path_prompt(
        &mut self,
        thread_id: ThreadId,
        name: KeyName,
        initial: String,
        error: Option<String>,
    ) {
        let tx = self.app_event_tx.clone();
        self.bottom_pane.show_text_prompt(CustomPromptView::new(
            format!("OpenRouter key · {name}"),
            "Absolute path to the key file".into(),
            initial,
            error.or_else(|| {
                Some(
                    "The file's contents stay on the app-server host. Never paste the key here."
                        .into(),
                )
            }),
            Box::new(move |input| {
                tx.send(AppEvent::OpenRouter(Event::KeyFileSubmitted {
                    thread_id,
                    name: name.clone(),
                    input,
                }));
            }),
        ));
    }

    pub(crate) fn openrouter_remove_key_confirm(&mut self, thread_id: ThreadId, name: KeyName) {
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("Remove OpenRouter key · {name}?")),
            subtitle: Some("Removes the configured key, not its file. Conversations using it will need another key.".into()),
            items: vec![
                SelectionItem { name: "Keep key".into(), dismiss_on_select: true, ..Default::default() },
                SelectionItem {
                    name: "Remove key".into(),
                    actions: vec![Box::new(move |tx| tx.send(AppEvent::OpenRouter(Event::Request(Request::RemoveKey { thread_id, name: name.clone() }))))],
                    dismiss_on_select: true, require_explicit_confirmation: true, ..Default::default()
                },
            ], ..SelectionViewParams::confirmation()
        });
    }

    pub(crate) fn append_openrouter_accounts(
        thread_id: ThreadId,
        response: &codex_app_server_protocol::ThreadAccountListResponse,
        items: &mut Vec<SelectionItem>,
    ) {
        for status in &response.openrouter_keys {
            let name = status.key.name.clone();
            let remove_name = name.clone();
            let pending = response.pending_openrouter_key.as_ref() == Some(&name);
            let description = match &status.state {
                OpenRouterKeyState::Read {
                    label,
                    balance,
                    spend,
                    ..
                } => {
                    let suffix: String = label
                        .chars()
                        .rev()
                        .take_while(char::is_ascii_alphanumeric)
                        .take(4)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    format!(
                        "key …{suffix} · {} this month{}",
                        spend.monthly,
                        balance
                            .map(|amount| format!(" · {amount} balance"))
                            .unwrap_or_default()
                    )
                }
                OpenRouterKeyState::Failed { error } => format!("Unavailable: {error}"),
            };
            items.push(SelectionItem {
                name: format!("OpenRouter · {name}"),
                description: Some(format!(
                    "{description}{}",
                    if pending { " · switch pending" } else { "" }
                )),
                is_current: response.selected_openrouter_key.as_ref() == Some(&name),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::SelectAccount {
                        thread_id,
                        account_id: name.to_string(),
                        provider: codex_app_server_protocol::AccountProvider::OpenRouter,
                    });
                })],
                secondary_action: Some(crate::bottom_pane::SelectionSecondaryAction {
                    key: key_hint::ctrl(KeyCode::Char('d')),
                    footer_hint: "enter select · ctrl+d remove · esc back".into(),
                    action: Box::new(move |tx| {
                        tx.send(AppEvent::OpenRouter(Event::RemoveKeyConfirm {
                            thread_id,
                            name: remove_name.clone(),
                        }));
                    }),
                }),
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        items.push(SelectionItem {
            name: "Add OpenRouter key…".into(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenRouter(Event::KeyNamePrompt { thread_id }));
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
    }

    pub(super) fn reset_openrouter_status(&mut self, thread_changed: bool) {
        if thread_changed {
            self.openrouter_ui.request = None;
        }
        self.openrouter_ui.status = None;
        self.openrouter_ui.status_request = None;
        self.openrouter_ui.last_refresh = None;
        self.refresh_openrouter_status(StatusRefresh::Now);
    }

    pub(crate) fn refresh_openrouter_status(&mut self, mode: StatusRefresh) -> bool {
        if !self.on_openrouter() {
            return false;
        }
        let Some(thread_id) = self.thread_id else {
            return false;
        };
        if self.openrouter_ui.status_request.is_some() && mode != StatusRefresh::Show {
            return true;
        }
        if mode == StatusRefresh::AfterTurn
            && self
                .openrouter_ui
                .last_refresh
                .is_some_and(|last| last.elapsed() < Duration::from_mins(1))
        {
            return true;
        }
        let id = uuid::Uuid::new_v4();
        self.openrouter_ui.status_request = Some(id);
        self.openrouter_ui.last_refresh = Some(Instant::now());
        if mode == StatusRefresh::Show {
            self.add_status_output(false, None);
        }
        self.app_event_tx
            .send(AppEvent::OpenRouter(Event::RefreshStatus {
                thread_id,
                id,
                mode,
            }));
        true
    }

    pub(crate) fn finish_openrouter_status(
        &mut self,
        thread_id: ThreadId,
        id: uuid::Uuid,
        mode: StatusRefresh,
        result: &Result<OpenRouterKeyStatus, StatusFailure>,
    ) {
        if self.thread_id != Some(thread_id) || self.openrouter_ui.status_request != Some(id) {
            return;
        }
        self.openrouter_ui.status_request = None;
        self.openrouter_ui.status = result.as_ref().ok().cloned();
        if let Some(status) = &self.openrouter_ui.status {
            self.active_account_label = Some(format!("OpenRouter · {}", status.key.name));
        }
        if mode == StatusRefresh::Show {
            let spend = self
                .token_info
                .as_ref()
                .and_then(|info| info.total_token_usage.cost);
            let status = result.as_ref().map_err(ToString::to_string);
            self.add_to_history(PlainHistoryCell::new(views::status_lines(
                status.as_ref().copied().map_err(String::as_str),
                spend,
            )));
        } else {
            match &result {
                Err(error @ StatusFailure::Removed(_)) => {
                    self.add_info_message(error.to_string(), None);
                }
                Err(StatusFailure::Failed(error))
                | Ok(OpenRouterKeyStatus {
                    state: OpenRouterKeyState::Failed { error },
                    ..
                }) => {
                    self.add_error_message(format!("Cannot refresh OpenRouter key: {error}"));
                }
                Ok(OpenRouterKeyStatus {
                    state: OpenRouterKeyState::Read { .. },
                    ..
                }) => {}
            }
        }
        self.refresh_status_line();
        self.request_redraw();
    }

    pub(crate) fn show_openrouter_account_selection(&mut self, name: &str) {
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(ACCOUNT_SELECTION_VIEW),
            title: Some(format!("OpenRouter · {name}")),
            items: vec![SelectionItem {
                name: "Selecting key…".into(),
                is_disabled: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        });
    }

    pub(crate) fn finish_openrouter_account_selection(
        &mut self,
        result: &Result<codex_app_server_protocol::ThreadAccountSelectResponse, String>,
    ) -> bool {
        let visible = self.bottom_pane.active_view_id() == Some(ACCOUNT_SELECTION_VIEW);
        self.bottom_pane.dismiss_view_by_id(ACCOUNT_SELECTION_VIEW);
        if visible && let Err(error) = result {
            self.show_openrouter_error(format!("Cannot change OpenRouter key: {error}"));
            return true;
        }
        false
    }

    pub(crate) fn openrouter_request_pending(&mut self) -> bool {
        if self.openrouter_ui.request.is_some() {
            self.show_openrouter_error(
                "An OpenRouter request is still awaiting confirmation.".into(),
            );
            true
        } else {
            false
        }
    }

    pub(crate) fn openrouter_account_label(&self) -> Option<String> {
        self.loaded_account()?;
        if !self.on_openrouter() {
            return None;
        }
        self.openrouter_ui
            .status
            .as_ref()
            .map(|status| format!("OpenRouter · {}", status.key.name))
    }

    pub(super) fn openrouter_balance_value(&self) -> Option<String> {
        let status = self.openrouter_ui.status.as_ref()?;
        match &status.state {
            OpenRouterKeyState::Read { balance, limit, .. } => {
                let remaining = match (balance, limit) {
                    (Some(balance), Some(limit)) => Some((*balance).min(limit.remaining)),
                    (balance, None) => *balance,
                    (None, Some(limit)) => Some(limit.remaining),
                };
                remaining.map(|amount| format!("{amount} left"))
            }
            OpenRouterKeyState::Failed { .. } => None,
        }
    }

    pub(super) fn openrouter_daily_spend_value(&self) -> Option<String> {
        match &self.openrouter_ui.status.as_ref()?.state {
            OpenRouterKeyState::Read { spend, .. } => Some(format!("{} today", spend.daily)),
            OpenRouterKeyState::Failed { .. } => None,
        }
    }
}
