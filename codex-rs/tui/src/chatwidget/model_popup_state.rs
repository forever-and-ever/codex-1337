//! Accept current account-scoped model replies and refresh present pickers without reopening them.
//! Preserve highlighted models by wire slug, independent of their display names.

use super::model_popups::ALL_MODELS_SELECTION_VIEW_ID;
use super::model_popups::MODEL_LOADING_VIEW_ID;
use super::model_popups::MODEL_OPTIONS_VIEW_ID;
use super::model_popups::MODEL_SELECTION_VIEW_ID;
use super::*;

pub(super) const EFFORT_LOADING_VIEW: &str = "effort-loading";

pub(super) enum ModelPopupRequest {
    Refresh(uuid::Uuid),
    Effort {
        id: uuid::Uuid,
        provider: String,
        thread: Option<ThreadId>,
        model: String,
    },
}

impl ModelPopupRequest {
    pub(super) fn id(&self) -> uuid::Uuid {
        match self {
            Self::Refresh(id) | Self::Effort { id, .. } => *id,
        }
    }
}

impl ChatWidget {
    pub(crate) fn model_picker_unavailable(&self) -> Option<String> {
        match self.model_catalog.provider_write.try_lock() {
            Ok(changed) => changed.then(|| "The model list may be out of date. Restart the app server and TUI to refresh /model.".into()),
            Err(_) => Some("Saving the default provider. Try /model after the save finishes.".into()),
        }
    }

    pub(super) fn scope_model_picker_event(&self) -> impl Fn(AppEvent) -> AppEvent + use<> {
        let backend = self.model_catalog.backend;
        let thread_id = self.thread_id;
        move |event| match backend {
            Some(backend) => AppEvent::ScopedModelPicker {
                backend,
                thread_id,
                event: Box::new(event),
            },
            None => event,
        }
    }

    pub(crate) fn dismiss_stale_model_picker(&mut self) {
        self.model_popup_request = None;
        let had_popup = !self.no_modal_or_popup_active();
        for id in [
            EFFORT_LOADING_VIEW,
            MODEL_LOADING_VIEW_ID,
            MODEL_OPTIONS_VIEW_ID,
            ALL_MODELS_SELECTION_VIEW_ID,
            MODEL_SELECTION_VIEW_ID,
        ] {
            while self.bottom_pane.dismiss_view_by_id(id) {}
        }
        if had_popup && self.no_modal_or_popup_active() {
            self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
        }
    }

    pub(super) fn set_model_refresh_request(&mut self, request_id: uuid::Uuid) {
        self.bottom_pane.dismiss_view_by_id(EFFORT_LOADING_VIEW);
        self.model_popup_request = Some(ModelPopupRequest::Refresh(request_id));
    }

    pub(crate) fn cancel_model_request(&mut self, request_id: uuid::Uuid) {
        if self
            .model_popup_request
            .as_ref()
            .is_some_and(|request| request.id() == request_id)
        {
            self.model_popup_request = None;
            self.bottom_pane.dismiss_view_by_id(EFFORT_LOADING_VIEW);
        }
    }

    pub(super) fn refresh_account_scoped_models(&mut self) {
        if self.loaded_account().is_none() {
            return;
        }
        if self.model_catalog.provider_may_have_changed() {
            return;
        }
        // Identity, not account kind, determines whether cached access programs are authoritative.
        for model in &mut Arc::make_mut(&mut self.model_catalog).models {
            model.available_access_programs = None;
        }
        let request_id = uuid::Uuid::new_v4();
        self.set_model_refresh_request(request_id);
        self.app_event_tx.send(AppEvent::FetchModels { request_id });
    }

    fn model_popup_view_id(&self) -> Option<&'static str> {
        [MODEL_SELECTION_VIEW_ID, ALL_MODELS_SELECTION_VIEW_ID]
            .into_iter()
            .find(|view_id| {
                self.bottom_pane
                    .selected_index_for_present_view(view_id)
                    .is_some()
            })
    }

    pub(crate) fn model_popup_request_is_current(&self, request_id: uuid::Uuid) -> bool {
        match self.model_popup_request.as_ref() {
            Some(ModelPopupRequest::Refresh(id)) => *id == request_id,
            Some(ModelPopupRequest::Effort {
                id,
                provider,
                thread,
                model,
            }) => {
                *id == request_id
                    && *provider == self.config.model_provider_id
                    && *thread == self.thread_id
                    && model == self.current_model()
                    && self.bottom_pane.active_view_id() == Some(EFFORT_LOADING_VIEW)
            }
            None => false,
        }
    }

    pub(crate) fn on_models_loaded(
        &mut self,
        request_id: uuid::Uuid,
        result: Result<crate::model_catalog::ModelListing, String>,
    ) -> bool {
        if self.model_catalog.provider_may_have_changed()
            || !self.model_popup_request_is_current(request_id)
        {
            self.cancel_model_request(request_id);
            return false;
        }
        let effort_model = match self.model_popup_request.take() {
            Some(ModelPopupRequest::Effort { model, .. }) => {
                self.bottom_pane.dismiss_view_by_id(EFFORT_LOADING_VIEW);
                Some(model)
            }
            Some(ModelPopupRequest::Refresh(_)) => None,
            None => unreachable!("a current model request must be pending"),
        };
        let loading = self.bottom_pane.active_view_id() == Some(MODEL_LOADING_VIEW_ID);
        self.bottom_pane.dismiss_view_by_id(MODEL_LOADING_VIEW_ID);
        let listing = match result {
            Ok(listing) => listing,
            Err(error) => {
                if loading {
                    self.add_error_message("Could not load models. Try /model again.".into());
                }
                if effort_model.is_some() {
                    self.add_error_message(format!("Cannot load reasoning choices: {error}"));
                }
                return false;
            }
        };
        let effort_preset = effort_model.as_ref().and_then(|model| {
            listing
                .models
                .iter()
                .find(|preset| &preset.model == model)
                .cloned()
        });
        let changed = !((listing.models.is_empty() || self.model_catalog.models == listing.models)
            && self.model_catalog.providers == listing.providers);
        if changed {
            let catalog = Arc::make_mut(&mut self.model_catalog);
            if !listing.models.is_empty() {
                catalog.models = listing.models;
            }
            catalog.providers = listing.providers;
            self.set_daybreak_enabled(self.daybreak_enabled);
            self.refresh_effective_service_tier();
            self.refresh_model_dependent_surfaces();
        }
        if let Some(model) = effort_model {
            if let Some(preset) = effort_preset {
                self.open_reasoning_popup(preset);
            } else {
                self.add_error_message(format!("No reasoning choices were returned for {model}."));
            }
        }
        if loading {
            self.open_model_popup_with_presets(self.model_catalog.models.clone());
            if self.no_modal_or_popup_active() {
                self.on_modal_or_popup_closed();
            }
        }
        changed
    }

    /// Keep a present model picker aligned with the active task, including Reserve entry/exit.
    pub(super) fn refresh_open_model_picker(&mut self) {
        // Refresh an existing parent without reopening it or interrupting its reasoning child.
        match self.model_popup_view_id() {
            Some(MODEL_SELECTION_VIEW_ID) => self.open_model_popup_with_presets(
                self.model_catalog.try_list_models().unwrap_or_default(),
            ),
            Some(ALL_MODELS_SELECTION_VIEW_ID) => self.open_all_models_popup(),
            _ => {}
        }
    }

    pub(super) fn show_model_selection_view(
        &mut self,
        model_ids: Vec<String>,
        mut params: SelectionViewParams,
    ) {
        let selected_index = params
            .view_id
            .and_then(|view_id| self.bottom_pane.selected_index_for_present_view(view_id));
        let selected_model = selected_index.and_then(|index| self.model_popup_model_ids.get(index));
        params.initial_selected_idx = model_ids
            .iter()
            .position(|model| Some(model) == selected_model);
        self.model_popup_model_ids = model_ids;
        if let Some(view_id) = params.view_id.filter(|_| selected_index.is_some()) {
            self.bottom_pane
                .replace_selection_view_if_present(view_id, params);
        } else {
            self.bottom_pane.show_selection_view(params);
        }
    }
}
