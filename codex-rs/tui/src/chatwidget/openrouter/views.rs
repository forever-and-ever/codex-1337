use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::ViewCompletion;
use crate::bottom_pane::ViewPresentation;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::KeymapContextSet;
use crate::keymap::ListKeymap;
use crate::openrouter::Event;
use crate::openrouter::ModelMenu;
use crate::openrouter::Request;
use crate::openrouter::VIEW;
use crate::render::renderable::Renderable;
use codex_app_server_protocol::OpenRouterHost;
use codex_app_server_protocol::OpenRouterKeyState;
use codex_app_server_protocol::OpenRouterKeyStatus;
use codex_app_server_protocol::OpenRouterModelEndpointsResponse;
use codex_protocol::EndpointFilter;
use codex_protocol::EndpointSet;
use codex_protocol::EndpointSlug;
use codex_protocol::ModelSlug;
use codex_protocol::Usd;
use codex_protocol::openai_models::ModelPreset;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::collections::BTreeSet;

pub(super) struct ModelView {
    inner: ListSelectionView,
    models: Vec<ModelPreset>,
    menu: ModelMenu,
    tx: AppEventSender,
}

impl ModelView {
    pub(super) fn new(
        params: SelectionViewParams,
        models: Vec<ModelPreset>,
        menu: ModelMenu,
        tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        Self {
            inner: ListSelectionView::new(params, tx.clone(), keymap),
            models,
            menu,
            tx,
        }
    }
}

impl BottomPaneView for ModelView {
    fn presentation(&self) -> ViewPresentation {
        self.inner.presentation()
    }
    fn view_id(&self) -> Option<&'static str> {
        Some(VIEW)
    }
    fn selected_index(&self) -> Option<usize> {
        self.inner.selected_index()
    }
    fn keymap_contexts(&self) -> KeymapContextSet {
        self.inner.keymap_contexts()
    }
    fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }
    fn completion(&self) -> Option<ViewCompletion> {
        self.inner.completion()
    }
    fn dismiss_after_child_accept(&self) -> bool {
        self.inner.dismiss_after_child_accept()
    }
    fn clear_dismiss_after_child_accept(&mut self) {
        self.inner.clear_dismiss_after_child_accept();
    }
    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.inner.on_ctrl_c()
    }
    fn handle_key_event(&mut self, event: KeyEvent) {
        if event.modifiers == KeyModifiers::CONTROL
            && matches!(event.code, KeyCode::Char('p' | 'd'))
        {
            let Some(model) = self
                .inner
                .selected_index()
                .and_then(|index| self.models.get(index))
            else {
                return;
            };
            let slug = match model
                .model
                .strip_suffix(":batch")
                .unwrap_or(&model.model)
                .parse::<ModelSlug>()
            {
                Ok(slug) => slug,
                Err(error) => {
                    self.tx.send(AppEvent::OpenRouter(Event::Error(error)));
                    return;
                }
            };
            let menu = self.menu.clone();
            let request = match event.code {
                KeyCode::Char('p') if matches!(slug, ModelSlug::Preset(_)) => {
                    self.tx.send(AppEvent::OpenRouter(Event::Error(
                        "Presets control their own endpoint routing.".into(),
                    )));
                    return;
                }
                KeyCode::Char('p') => Request::Endpoints { menu, slug },
                _ => Request::Forget { menu, slug },
            };
            self.tx.send(AppEvent::OpenRouter(Event::Request(request)));
        } else {
            self.inner.handle_key_event(event);
        }
    }
}

impl Renderable for ModelView {
    fn desired_height(&self, width: u16) -> u16 {
        self.inner.desired_height(width)
    }
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.inner.render(area, buf);
    }
}

struct HostRow {
    slug: EndpointSlug,
    name: String,
    description: String,
}

#[derive(Clone, Copy)]
enum EndpointMark {
    Allow,
    Block,
}

pub(super) struct EndpointsView {
    inner: ListSelectionView,
    menu: ModelMenu,
    slug: ModelSlug,
    rows: Vec<HostRow>,
    filter: EndpointFilter,
    tx: AppEventSender,
    keymap: ListKeymap,
}

impl EndpointsView {
    pub(super) fn new(
        menu: ModelMenu,
        slug: ModelSlug,
        response: OpenRouterModelEndpointsResponse,
        tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        let mut rows = response.hosts.iter().map(host_row).collect::<Vec<_>>();
        if let EndpointFilter::Only(set) | EndpointFilter::Ignore(set) = &response.filter {
            for saved in set.slugs() {
                if rows.iter().any(|row| row.slug == *saved) {
                    continue;
                }
                let offered = response
                    .hosts
                    .iter()
                    .flat_map(|host| &host.endpoints)
                    .any(|endpoint| saved.matches(endpoint.slug.as_str()));
                rows.push(HostRow {
                    slug: saved.clone(),
                    name: saved.to_string(),
                    description: if offered {
                        "Saved endpoint filter".into()
                    } else {
                        "(no longer offered)".into()
                    },
                });
            }
        }
        let params = endpoint_params(&slug, &rows, &response.filter, None);
        Self {
            inner: ListSelectionView::new(params, tx.clone(), keymap.clone()),
            menu,
            slug,
            rows,
            filter: response.filter,
            tx,
            keymap,
        }
    }

    fn toggle(&mut self, mark: EndpointMark) {
        let Some(row) = self
            .inner
            .selected_index()
            .and_then(|index| self.rows.get(index))
        else {
            return;
        };
        let mut slugs = match (&self.filter, mark) {
            (EndpointFilter::Only(set), EndpointMark::Allow)
            | (EndpointFilter::Ignore(set), EndpointMark::Block) => set.slugs().clone(),
            _ => BTreeSet::new(),
        };
        if !slugs.remove(&row.slug) {
            slugs.insert(row.slug.clone());
        }
        self.filter = match (EndpointSet::new(slugs), mark) {
            (Some(set), EndpointMark::Allow) => EndpointFilter::Only(set),
            (Some(set), EndpointMark::Block) => EndpointFilter::Ignore(set),
            (None, _) => EndpointFilter::All,
        };
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let params = endpoint_params(
            &self.slug,
            &self.rows,
            &self.filter,
            self.inner.selected_index(),
        );
        self.inner = ListSelectionView::new(params, self.tx.clone(), self.keymap.clone());
    }
}

impl BottomPaneView for EndpointsView {
    fn presentation(&self) -> ViewPresentation {
        self.inner.presentation()
    }
    fn view_id(&self) -> Option<&'static str> {
        Some(crate::openrouter::ENDPOINTS_VIEW)
    }
    fn selected_index(&self) -> Option<usize> {
        self.inner.selected_index()
    }
    fn keymap_contexts(&self) -> KeymapContextSet {
        self.inner.keymap_contexts()
    }
    fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }
    fn completion(&self) -> Option<ViewCompletion> {
        self.inner.completion()
    }
    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.inner.on_ctrl_c()
    }
    fn handle_key_event(&mut self, event: KeyEvent) {
        match event {
            KeyEvent {
                code: KeyCode::Char('w'),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.toggle(EndpointMark::Allow),
            KeyEvent {
                code: KeyCode::Char('b'),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.toggle(EndpointMark::Block),
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.filter = EndpointFilter::All;
                self.rebuild();
            }
            _ if self.keymap.accept.is_pressed(event) => {
                self.tx
                    .send(AppEvent::OpenRouter(Event::Request(Request::Route {
                        menu: self.menu.clone(),
                        slug: self.slug.clone(),
                        filter: self.filter.clone(),
                    })));
            }
            _ => self.inner.handle_key_event(event),
        }
    }
}

impl Renderable for EndpointsView {
    fn desired_height(&self, width: u16) -> u16 {
        self.inner.desired_height(width)
    }
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.inner.render(area, buf);
    }
}

fn host_row(host: &OpenRouterHost) -> HostRow {
    let quantizations = host
        .endpoints
        .iter()
        .filter_map(|endpoint| endpoint.quantization.as_deref())
        .filter(|quantization| !quantization.eq_ignore_ascii_case("unknown"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let tiers = host
        .endpoints
        .iter()
        .filter_map(|endpoint| endpoint.service_tier.as_deref())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|tier| match tier {
            "priority" => "Fast",
            "flex" => "Flex",
            "ultrafast" => "Ultra Fast",
            tier => tier,
        })
        .collect::<Vec<_>>()
        .join(", ");
    let input = host
        .endpoints
        .iter()
        .filter_map(|endpoint| endpoint.prompt_per_mtok)
        .min();
    let output = host
        .endpoints
        .iter()
        .filter_map(|endpoint| endpoint.completion_per_mtok)
        .min();
    let uptime = host
        .endpoints
        .iter()
        .filter_map(|endpoint| endpoint.uptime_percent)
        .max_by(f64::total_cmp);
    HostRow {
        slug: host.slug.clone(),
        name: host.name.clone(),
        description: [
            (!quantizations.is_empty()).then_some(quantizations),
            Some(format!(
                "{} in · {} out per M",
                input.map_or_else(|| "?".into(), |price| price.to_string()),
                output.map_or_else(|| "?".into(), |price| price.to_string())
            )),
            uptime.map(|uptime| format!("{uptime:.1}% uptime")),
            (!tiers.is_empty()).then_some(tiers),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · "),
    }
}

fn filter_label(rows: &[HostRow], filter: &EndpointFilter) -> String {
    let (prefix, set) = match filter {
        EndpointFilter::All => return "All endpoints".into(),
        EndpointFilter::Only(set) => ("Only", set),
        EndpointFilter::Ignore(set) => ("All except", set),
    };
    let names = set
        .slugs()
        .iter()
        .map(|slug| {
            rows.iter()
                .find(|row| row.slug == *slug)
                .map_or_else(|| slug.to_string(), |row| row.name.clone())
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{prefix} {names}")
}

fn endpoint_params(
    slug: &ModelSlug,
    rows: &[HostRow],
    filter: &EndpointFilter,
    selected: Option<usize>,
) -> SelectionViewParams {
    let label = filter_label(rows, filter);
    SelectionViewParams {
        view_id: Some(crate::openrouter::ENDPOINTS_VIEW),
        title: Some(format!("Endpoints · {slug}")),
        subtitle: Some(label),
        initial_selected_idx: selected,
        items: rows
            .iter()
            .map(|row| {
                let marker = match filter {
                    EndpointFilter::Only(set) if set.slugs().contains(&row.slug) => "[allow] ",
                    EndpointFilter::Ignore(set) if set.slugs().contains(&row.slug) => "[block] ",
                    _ => "",
                };
                SelectionItem {
                    name: format!("{marker}{}", row.name),
                    description: Some(row.description.clone()),
                    ..Default::default()
                }
            })
            .collect(),
        footer_hint: Some("w allow · b block · c clear · enter save · esc discard".into()),
        ..SelectionViewParams::picker()
    }
}

pub(super) fn status_lines(
    status: Result<&OpenRouterKeyStatus, &str>,
    conversation_spend: Option<Usd>,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("OpenRouter".bold())];
    match status {
        Err(error) => lines.push(Line::from(format!("  Unavailable: {error}"))),
        Ok(status) => {
            lines.push(Line::from(format!("  Key: {}", status.key.name)));
            if status.outstanding_batches != 0 {
                lines.push(Line::from(format!(
                    "  Unsettled batches: {} · final cost unknown",
                    status.outstanding_batches
                )));
            }
            match &status.state {
                OpenRouterKeyState::Failed { error } => {
                    lines.push(Line::from(format!("  Unavailable: {error}")));
                }
                OpenRouterKeyState::Read {
                    label,
                    balance,
                    limit,
                    spend,
                    ..
                } => {
                    lines.push(Line::from(format!("  Label: {label}")));
                    lines.push(Line::from(format!(
                        "  Balance: {}",
                        balance.map_or_else(|| "Unavailable".into(), |amount| amount.to_string())
                    )));
                    if let Some(limit) = limit {
                        lines.push(Line::from(format!(
                            "  Key limit: {} · {} remaining{}",
                            limit.limit,
                            limit.remaining,
                            limit
                                .reset
                                .as_ref()
                                .map(|reset| format!(" · resets {reset}"))
                                .unwrap_or_default()
                        )));
                    }
                    lines.push(Line::from(format!(
                        "  Spend: {} today · {} this week · {} this month",
                        spend.daily, spend.weekly, spend.monthly
                    )));
                }
            }
        }
    }
    lines.push(Line::from(format!(
        "  This conversation: {}",
        conversation_spend.map_or_else(|| "Unknown".into(), |amount| amount.to_string())
    )));
    lines
}
