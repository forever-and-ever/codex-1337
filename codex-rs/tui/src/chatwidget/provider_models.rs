//! Provider selection is explicitly a new conversation, not an in-place model switch.

use super::AppEvent;
use super::ChatWidget;
use super::ModelPreset;
use super::SelectionItem;
use super::SelectionViewParams;
use crate::app::ProviderModelChoice;
use crate::app::ProviderModelEvent;
use crate::model_catalog::ModelListing;
use codex_app_server_protocol::ModelProviderOption;

pub(super) const PROVIDER_MODELS_VIEW: &str = "provider-models";

impl ChatWidget {
    pub(super) fn prepend_provider_choices(
        &self,
        items: &mut Vec<SelectionItem>,
        model_ids: &mut Vec<String>,
    ) {
        for (index, (provider, choice)) in self.provider_choices().enumerate() {
            model_ids.insert(index, format!("provider:{}", provider.id));
            items.insert(index, choice);
        }
    }

    pub(super) fn provider_choices(
        &self,
    ) -> impl Iterator<Item = (&ModelProviderOption, SelectionItem)> {
        self.model_catalog
            .providers
            .iter()
            .filter(move |provider| provider.id != self.config.model_provider_id)
            .map(|provider| {
                let action_provider = provider.clone();
                (
                    provider,
                    SelectionItem {
                        name: format!("{}…", provider.name),
                        description: Some("Choose a model for a new conversation.".into()),
                        actions: vec![Box::new(move |tx| {
                            tx.send(AppEvent::ProviderModel(ProviderModelEvent::Open(
                                action_provider.clone(),
                            )));
                        })],
                        dismiss_on_select: true,
                        ..Default::default()
                    },
                )
            })
    }

    pub(crate) fn show_provider_models_loading(
        &mut self,
        request_id: uuid::Uuid,
        provider: &ModelProviderOption,
    ) {
        if self.refuse_pending_account_operation() {
            return;
        }
        self.set_model_refresh_request(request_id);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(PROVIDER_MODELS_VIEW),
            title: Some(format!("New {} conversation", provider.name)),
            items: vec![SelectionItem {
                name: "Loading models…".into(),
                is_disabled: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        });
    }

    pub(crate) fn show_provider_models(
        &mut self,
        request_id: uuid::Uuid,
        provider: &ModelProviderOption,
        result: Result<ModelListing, String>,
    ) {
        if !self.model_popup_request_is_current(request_id) {
            return;
        }
        self.model_popup_request = None;
        // Escape or navigation during discovery must not reopen a dismissed picker.
        let (models, error) = match result {
            Ok(listing) if !listing.models.is_empty() => (listing.models, None),
            Ok(_) => (
                Vec::new(),
                Some("No models are available for this provider.".to_string()),
            ),
            Err(error) => (Vec::new(), Some(error)),
        };
        let mut items = Vec::new();
        for model in models {
            let provider = provider.clone();
            items.push(SelectionItem {
                name: model.display_name.clone(),
                description: Some(model.description.clone()),
                actions: vec![Box::new(move |tx| {
                    // Reuse the provider picker for effort; never emit an in-place model update.
                    tx.send(AppEvent::OpenProviderReasoning {
                        provider: provider.clone(),
                        model: model.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        if let Some(error) = error {
            items.push(SelectionItem {
                name: error,
                is_disabled: true,
                ..Default::default()
            });
        }
        self.bottom_pane.replace_selection_view_if_present(
            PROVIDER_MODELS_VIEW,
            SelectionViewParams {
                view_id: Some(PROVIDER_MODELS_VIEW),
                title: Some(format!("New {} conversation", provider.name)),
                items,
                ..SelectionViewParams::picker()
            },
        );
    }

    pub(crate) fn show_provider_reasoning(
        &mut self,
        provider: &ModelProviderOption,
        model: &ModelPreset,
    ) {
        let mut efforts: Vec<_> = model
            .supported_reasoning_efforts
            .iter()
            .map(|preset| preset.effort.clone())
            .collect();
        if efforts.is_empty() {
            efforts.push(model.default_reasoning_effort.clone());
        }
        let initial_selected_idx = efforts
            .iter()
            .position(|effort| *effort == model.default_reasoning_effort);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("New {} conversation", provider.name)),
            subtitle: Some(model.display_name.clone()),
            initial_selected_idx,
            items: efforts
                .into_iter()
                .map(|effort| {
                    let choice = ProviderModelChoice {
                        provider: provider.clone(),
                        model: model.clone(),
                        effort: effort.clone(),
                    };
                    SelectionItem {
                        name: effort.to_string(),
                        actions: vec![Box::new(move |tx| {
                            tx.send(AppEvent::ProviderModel(ProviderModelEvent::Start(
                                Box::new(choice.clone()),
                            )));
                        })],
                        dismiss_on_select: true,
                        ..Default::default()
                    }
                })
                .collect(),
            ..SelectionViewParams::picker()
        });
    }
}
