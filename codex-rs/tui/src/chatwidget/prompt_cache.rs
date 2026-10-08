//! The /warm toggle and the prompt-cache state behind the composer's glyphs.
use super::*;
use crate::bottom_pane::PromptCacheGlyphs;
use codex_app_server_protocol::PromptCacheEstimate;
use codex_app_server_protocol::PromptCacheWarming;
use codex_app_server_protocol::ThreadPromptCacheMissedNotification;
use codex_app_server_protocol::ThreadPromptCacheUpdatedNotification;

impl ChatWidget {
    pub(super) fn toggle_warming(&mut self) {
        let Some(thread_id) = self.thread_id else {
            self.add_error_message(
                "Session is still starting; try /warm again shortly.".to_owned(),
            );
            return;
        };
        self.app_event_tx.send(AppEvent::SetWarming {
            thread_id,
            enabled: self.prompt_cache.warming == PromptCacheWarming::Off,
        });
    }

    pub(super) fn on_prompt_cache_updated(&mut self, update: ThreadPromptCacheUpdatedNotification) {
        self.prompt_cache = PromptCacheGlyphs {
            estimate: update.estimate,
            warming: update.warming,
        };
        // The snowflake appears at expiry without the server saying so.
        if let PromptCacheEstimate::Warm { expires_at } = update.estimate
            && let Ok(remaining) = u64::try_from(expires_at - chrono::Utc::now().timestamp())
        {
            self.frame_requester
                .schedule_frame_in(Duration::from_secs(remaining.saturating_add(1)));
        }
        self.request_redraw();
    }

    pub(super) fn on_prompt_cache_missed(&mut self, missed: ThreadPromptCacheMissedNotification) {
        let tokens = crate::status::format_tokens_compact(missed.rewritten_tokens);
        self.add_warning_message(if missed.while_warming {
            format!("Cache missed while warming: rewrote {tokens} tokens. Warming stopped.")
        } else {
            format!("Cache missed: rewrote {tokens} tokens.")
        });
    }
}
