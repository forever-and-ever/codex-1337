//! The composer's stand-in while rewinding: one line naming the mode and its keys.
//!
//! The view holds no selection and never completes itself, and the shortcut that began the rewind
//! does nothing here. Each key becomes a [`RewindInput`]
//! for `App` (`app/rewind.rs`), which owns the selection and dismisses the view when the
//! rewind ends; every owned draw also checks that the view is still stacked, because a cleared
//! stack would otherwise strand the rewind.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use super::BottomPane;
use super::BottomPaneView;
use super::CancellationEvent;
use super::menu_surface_padding_height;
use super::render_menu_surface;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::footer_hint::first_fitting_line;
use crate::key_hint;
use crate::render::renderable::Renderable;
use crate::style::accent_color;

const REWIND_VIEW_ID: &str = "rewind";

/// Each hint: its keys, then what they do.
const HINTS: [(&[KeyCode], &str); 3] = [
    (&[KeyCode::Up, KeyCode::Down], " prompt"),
    (&[KeyCode::Enter], " rewind"),
    (&[KeyCode::Esc], " cancel"),
];

/// A rewind key, as `App` receives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RewindInput {
    /// Select the previous prompt.
    Older,
    /// Select the next prompt.
    Newer,
    /// Revert before the selected prompt and restore it to the composer.
    Confirm,
    /// Leave without reverting.
    Cancel,
}

struct RewindView {
    app_event_tx: AppEventSender,
}

impl RewindView {
    fn line(width: u16) -> Line<'static> {
        let title = || "Rewinding".bold().fg(accent_color());
        let mut spans = vec![title(), "  ".into()];
        for (index, (keys, action)) in HINTS.into_iter().enumerate() {
            if index > 0 {
                spans.push(" · ".dim());
            }
            spans.extend(keys.iter().map(|&key| Span::from(key_hint::plain(key))));
            spans.push(action.dim());
        }
        first_fitting_line([Line::from(spans), Line::from(title())], width)
    }
}

impl Renderable for RewindView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let area = render_menu_surface(area, buf);
        Self::line(area.width).render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        1 + menu_surface_padding_height()
    }
}

impl BottomPaneView for RewindView {
    fn handle_key_event(&mut self, key: KeyEvent) {
        // Held Enter or Escape must not repeat into the composer the rewind hands back.
        let press = key.kind == KeyEventKind::Press;
        let input = if press && key_hint::plain(KeyCode::Esc).is_press(key) {
            RewindInput::Cancel
        } else if press && key_hint::plain(KeyCode::Enter).is_press(key) {
            RewindInput::Confirm
        } else if key_hint::plain(KeyCode::Up).is_press(key) {
            RewindInput::Older
        } else if key_hint::plain(KeyCode::Down).is_press(key) {
            RewindInput::Newer
        } else {
            return;
        };
        self.app_event_tx.send(AppEvent::Rewind(input));
    }

    fn view_id(&self) -> Option<&'static str> {
        Some(REWIND_VIEW_ID)
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.app_event_tx
            .send(AppEvent::Rewind(RewindInput::Cancel));
        CancellationEvent::Handled
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
}

impl BottomPane {
    /// Replace the composer with the rewind view.
    pub(crate) fn show_rewind(&mut self) {
        let view = RewindView {
            app_event_tx: self.app_event_tx.clone(),
        };
        self.show_view(Box::new(view));
    }

    pub(crate) fn dismiss_rewind(&mut self) {
        self.dismiss_view_by_id(REWIND_VIEW_ID);
    }

    /// Anywhere in the stack: an approval may cover the rewind view without ending the rewind.
    pub(crate) fn shows_rewind(&self) -> bool {
        self.view_stack
            .iter()
            .any(|view| view.view_id() == Some(REWIND_VIEW_ID))
    }
}
