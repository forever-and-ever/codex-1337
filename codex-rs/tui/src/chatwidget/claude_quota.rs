use super::ChatWidget;
use crate::app_event::AppEvent;
use crate::status::StatusAccountDisplay;
use crate::status::claude_quota_data;
use chrono::Local;
use codex_app_server_protocol::AccountQuota;

impl ChatWidget {
    pub(super) fn request_status_with_claude_quota(&mut self) -> bool {
        let Some(StatusAccountDisplay::ClaudeSubscription { email }) =
            self.status_account_display()
        else {
            return false;
        };
        let account = email.clone();
        let request_id = self.next_status_refresh_request_id;
        self.next_status_refresh_request_id = self.next_status_refresh_request_id.wrapping_add(1);
        self.add_status_output(/*refreshing_rate_limits*/ true, Some(request_id));
        self.app_event_tx.send(AppEvent::RefreshClaudeQuota {
            request_id,
            account,
        });
        true
    }

    pub(crate) fn finish_status_claude_quota_refresh(
        &mut self,
        request_id: u64,
        result: &Result<Option<AccountQuota>, String>,
    ) {
        let data = claude_quota_data(
            result.as_ref().ok().and_then(Option::as_ref),
            Local::now(),
            self.clock_format,
        );
        let mut remaining = Vec::with_capacity(self.refreshing_status_outputs.len());
        let mut updated_any = false;
        for (pending_request_id, handle) in self.refreshing_status_outputs.drain(..) {
            if pending_request_id == request_id {
                updated_any = true;
                handle.finish_claude_quota_refresh(data.clone());
            } else {
                remaining.push((pending_request_id, handle));
            }
        }
        self.refreshing_status_outputs = remaining;
        if updated_any {
            self.flush_status_outputs();
            self.request_redraw();
        }
    }
}
