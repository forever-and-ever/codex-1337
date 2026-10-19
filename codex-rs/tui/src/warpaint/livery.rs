//! Thread livery, mounted as upstream's `thread_color`. Threads take livery slots in order of
//! first appearance, so the first threads seen never share a color while the pack has colors
//! left; the pack decides each slot's color at flush.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::PoisonError;

use codex_protocol::ThreadId;
use ratatui::style::Color;

static SLOTS: LazyLock<Mutex<HashMap<ThreadId, usize>>> = LazyLock::new(Mutex::default);

pub(crate) fn thread_color(thread_id: ThreadId) -> Color {
    let mut slots = SLOTS.lock().unwrap_or_else(PoisonError::into_inner);
    let next = slots.len();
    crate::warpaint::livery(*slots.entry(thread_id).or_insert(next))
}
