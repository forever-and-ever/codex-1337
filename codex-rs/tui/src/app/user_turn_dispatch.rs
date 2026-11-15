//! UI admission for a user turn whose destination was captured before it entered the mailbox.

use super::App;
use super::AppCommand;
use super::AppRunControl;
use super::AppServerSession;
use super::Result;
use super::ThreadId;
use super::TypedRequestError;
use crate::app_server_session::UnsupportedLegacyPermissionProfile;
use crate::tui;

impl App {
    pub(super) async fn dispatch_captured_user_turn(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        mut op: AppCommand,
    ) -> Result<AppRunControl> {
        let displayed = self.chat_widget.thread_id() == Some(thread_id);
        if displayed {
            if self.chat_widget.defer_pending_turn_for_model_settings() {
                return Ok(AppRunControl::Continue);
            }
            self.chat_widget
                .apply_automatic_model_switch_to_pending_turn(&mut op);
            let screen_size = tui.terminal.last_known_screen_size;
            self.handle_draw_pre_render(tui, screen_size)?;
            if self.transcript_reflow.has_pending_reflow() {
                self.transcript_reflow.schedule_immediate();
                self.maybe_run_resize_reflow(tui, screen_size)?;
            }
            self.chat_widget.pre_draw_tick();
            self.render_chat_widget_frame(tui, screen_size)?;
            self.chat_widget.prepare_local_op_submission(&op);
        }
        let replaces_pending_steer = matches!(
            &op,
            AppCommand::UserTurn {
                pending_steer_replacement: Some(_),
                ..
            }
        );
        if let Err(error) = self.submit_thread_op(server, thread_id, op).await {
            if displayed && replaces_pending_steer {
                self.chat_widget.abort_pending_steer_replacement();
            }
            if self.recover_transport_error(&error) {
                return Ok(AppRunControl::Continue);
            }
            let unsupported_permissions = error
                .downcast_ref::<UnsupportedLegacyPermissionProfile>()
                .is_some();
            if displayed && unsupported_permissions {
                self.chat_widget.set_queue_autosend_suppressed(true);
            }
            let handled = displayed
                && (matches!(
                    error.downcast_ref::<TypedRequestError>(),
                    Some(TypedRequestError::Server { method, .. }) if method == "turn/start"
                ) || unsupported_permissions)
                && self
                    .chat_widget
                    .handle_turn_start_rejection(format!("Failed to start turn: {error:#}"));
            if !handled {
                return Err(error);
            }
            tracing::error!(error = ?error, "failed to start turn through app server");
        }
        Ok(AppRunControl::Continue)
    }
}
