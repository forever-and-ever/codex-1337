use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::ComposerState;
use super::EmptyStateAnimation;

impl EmptyStateAnimation {
    pub(super) fn render_landing_page(
        &mut self,
        available: Rect,
        buffer: &mut Buffer,
        composer: Option<ComposerState>,
    ) {
        // Fresh fullscreen presentation has neither a replay hitbox nor an animation clock.
        // Onboarding keeps its independent animated renderer.
        self.cancel_replay();
        if composer == Some(ComposerState::Empty) {
            crate::landing_page::render(available, buffer);
        }
    }
}
