//! Footer hints are a projection of the matcher's one pending chord, never another guide state.

use super::CHORD_REVEAL_DELAY;
use super::KEY_CHORD_TIMEOUT;
use super::KeyChordMatcher;
use super::RuntimeChordKeymap;

impl KeyChordMatcher {
    pub(crate) fn pending_hint_items(
        &self,
        keymap: &RuntimeChordKeymap,
        width: u16,
    ) -> Option<Vec<(String, String)>> {
        let pending = self.pending?;
        let elapsed = pending.started_at.elapsed();
        if elapsed < CHORD_REVEAL_DELAY || (!pending.persistent && elapsed >= KEY_CHORD_TIMEOUT) {
            return None;
        }
        let mut items = Vec::new();
        for (action, keys) in pending.actions(keymap) {
            items.push((
                keys.join(" / "),
                crate::keymap_setup::action_label(action.action),
            ));
            if crate::bottom_pane::footer_hint_items_line(&items).width() > usize::from(width) {
                items.pop();
                break;
            }
        }
        if items.is_empty() {
            return None;
        }
        items.push(("Esc".into(), "cancel".into()));
        if crate::bottom_pane::footer_hint_items_line(&items).width() > usize::from(width) {
            items.pop();
        }
        Some(items)
    }
}
